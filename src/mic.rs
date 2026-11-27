// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! `mic`: a microphone in the guest that plays an audio file from the host, so an app's dictation
//! is tested with no microphone of the Mac's: the audio server's virtual devices on Linux,
//! BlackHole on macOS, QEMU's sound card fed over D-Bus on Windows (`dbus.rs`).

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use crate::vm::{Os, Vm};
use crate::{bad, dbus, detach, qemu, run, ssh, system, unix_ms};

/// The microphone's samples: 16-bit little-endian mono PCM at 48 kHz.
const RATE: u32 = 48_000;

/// BlackHole 2ch (ExistentialAudio/BlackHole, GPL-3.0), an AudioServerPlugIn: its signed pkg, as
/// Homebrew's `blackhole-2ch` cask pins it.
const BLACKHOLE: (&str, &str) = (
  "https://existential.audio/downloads/BlackHole2ch-0.7.1.pkg",
  "57b540f27a3e29c37e310e01bee0fdfab76733087e47f997ef9dccf851400dcf",
);

/// Exits 3 unless BlackHole's version is in the guest.
const MACOS_HAS: &str = r#"v=$(plutil -extract CFBundleShortVersionString raw \
  /Library/Audio/Plug-Ins/HAL/BlackHole2ch.driver/Contents/Info.plist 2>/dev/null) || exit 3
[ "$v" = 0.7.1 ] || exit 3
"#;

/// Installs BlackHole's pkg, on stdin; coreaudiod loads it once restarted, which its scripts don't.
const MACOS_INSTALL: &str = r#"set -e
d=$(mktemp -d)
cat > "$d/BlackHole2ch.pkg"
sudo -n installer -pkg "$d/BlackHole2ch.pkg" -target / >/dev/null
rm -rf "$d"
sudo -n killall coreaudiod
"#;

/// Plays the WAV file on stdin to the default output, BlackHole, whose input is the microphone.
const MACOS_PLAY: &str = r#"set -e
d=$(mktemp -d)
cat > "$d/mic.wav"
afplay "$d/mic.wav"
rm -rf "$d"
"#;

/// PulseAudio's modules, which `pipewire-pulse` loads too: `trivm-mic`, a mono source of the
/// monitor of `trivm-mic-in`, which files play into; `trivm-speaker`, the default output, keeps the
/// guest's sounds out of it, where `module-always-sink` would put them.
const MODULES: [&str; 3] = [
  r#"module-null-sink sink_name=trivm-speaker sink_properties='device.description="trivm speaker"'"#,
  r#"module-null-sink sink_name=trivm-mic-in channels=1 channel_map=mono sink_properties='device.description="trivm microphone input"'"#,
  r#"module-remap-source master=trivm-mic-in.monitor source_name=trivm-mic channels=1 channel_map=mono source_properties='device.description="trivm microphone"'"#,
];

/// Starts the user's audio server, PipeWire's or PulseAudio, by its socket: the session that
/// installed it didn't. Exits 3 unless the microphone is there.
const LINUX_HAS: &str = r#"systemctl --user start pipewire-pulse.socket 2>/dev/null ||
  systemctl --user start pulseaudio.socket 2>/dev/null || true
pactl list short sources 2>/dev/null | grep -q "	trivm-mic	" || exit 3
"#;

/// Puts the modules in the audio server's config, for each login, and loads them now unless the
/// server, started by `pactl` here, read them; makes the microphone the default input and the
/// speaker the default output.
const LINUX_INSTALL: &str = r#"set -e
if ! command -v pactl >/dev/null; then
  sudo -n apt-get install -y pulseaudio-utils >/dev/null 2>&1 ||
    sudo -n dnf install -y -q pulseaudio-utils >/dev/null 2>&1
fi
if [ -f /etc/pulse/default.pa ]; then
  sudo -n mkdir -p /etc/pulse/default.pa.d
  printf '%s\n' "$PA" | sudo -n tee /etc/pulse/default.pa.d/trivm-mic.pa >/dev/null
fi
if [ -d /usr/share/pipewire ]; then
  sudo -n mkdir -p /etc/pipewire/pipewire-pulse.conf.d
  printf '%s\n' "$PW" | sudo -n tee /etc/pipewire/pipewire-pulse.conf.d/trivm-mic.conf >/dev/null
fi
# A server started only now has read them.
if ! pactl list short sources | grep -q "	trivm-mic	"; then
$LOAD
fi
pactl set-default-sink trivm-speaker
pactl set-default-source trivm-mic
"#;

/// Plays the samples on stdin into the microphone.
const LINUX_PLAY: &str = "exec pacat -d trivm-mic-in --format=s16le --rate=48000 --channels=1\n";

/// `mic <vm> [file]`: the microphone put in the guest if it isn't there, then the file played into
/// it, in real time.
pub fn mic(name: &str, file: Option<&str>) -> io::Result<()> {
  let vm = Vm::open(name)?;
  if vm.pid().is_none() {
    return Err(bad(format!("{name} isn't running; trivm start {name}")));
  }

  let wav = file.map(convert).transpose()?;
  match vm.config.os() {
    Os::Linux => guest(&vm, LINUX_HAS, LINUX_PLAY, wav.as_deref().map(samples).transpose()?),
    Os::Macos => guest(&vm, MACOS_HAS, MACOS_PLAY, wav.as_deref()),
    Os::Windows => windows(&vm, wav.as_deref().map(samples).transpose()?),
  }
}

/// The file as a WAV file of the microphone's samples, by macOS's `afconvert`, which reads any
/// format Core Audio does.
fn convert(file: &str) -> io::Result<Vec<u8>> {
  if !fs::metadata(file).is_ok_and(|x| x.is_file()) {
    return Err(bad(format!("{file}: no such file")));
  }

  let tmp = env::temp_dir().join(format!("trivm-mic-{}-{}.wav", std::process::id(), unix_ms()));
  let fmt = format!("LEI16@{RATE}");
  let res =
    run(Command::new("afconvert").args(["-f", "WAVE", "-d", &fmt, "-c", "1", file]).arg(&tmp));
  let wav = res.and_then(|_| fs::read(&tmp));
  let _ = fs::remove_file(&tmp);
  wav.map_err(|e| bad(format!("{file}: not an audio file macOS reads ({e})")))
}

/// The samples of a WAV file: its `data` chunk, after the chunks before it.
fn samples(wav: &[u8]) -> io::Result<&[u8]> {
  if wav.len() < 12 || &wav[..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
    return Err(bad("not a WAV file"));
  }

  let mut at = 12;
  while at + 8 <= wav.len() {
    let size = u32::from_le_bytes([wav[at + 4], wav[at + 5], wav[at + 6], wav[at + 7]]) as usize;
    let body = at + 8;
    if &wav[at..at + 4] == b"data" {
      return Ok(&wav[body..(body + size).min(wav.len())]);
    }

    at = body + size + size % 2;
  }

  Err(bad("a WAV file with no samples"))
}

/// The microphone put in a macOS or Linux guest when `has` says it isn't there, then `data` played
/// by `play` from stdin.
fn guest(vm: &Vm, has: &str, play: &str, data: Option<&[u8]>) -> io::Result<()> {
  let status = ssh::ssh(vm)?.arg("--").arg(has).stdin(Stdio::null()).status()?;
  match status.code() {
    Some(0) => {}
    Some(3) => install(vm)?,
    _ => return Err(bad(format!("checking {}'s microphone failed ({status})", vm.name))),
  }

  let Some(data) = data else { return Ok(()) };
  let status = feed(vm, play, data)?;
  match status.success() {
    true => Ok(()),
    false => Err(bad(format!("playing into {}'s microphone failed ({status})", vm.name))),
  }
}

/// Puts the microphone in the guest: BlackHole on macOS, the audio server's modules on Linux.
fn install(vm: &Vm) -> io::Result<()> {
  eprintln!("putting a microphone in {}", vm.name);
  let status = match vm.config.os() {
    Os::Macos => {
      let pkg = system::download(BLACKHOLE.0, BLACKHOLE.1)?;
      feed(vm, MACOS_INSTALL, &fs::read(pkg)?)?
    }
    _ => ssh::ssh(vm)?.arg("--").arg(linux_install()).stdin(Stdio::null()).status()?,
  };

  match status.success() {
    true => Ok(()),
    false => Err(bad(format!("putting a microphone in {} failed ({status})", vm.name))),
  }
}

/// `LINUX_INSTALL` with PulseAudio's config (`PA`), PipeWire's (`PW`) and the modules loaded.
fn linux_install() -> String {
  let pa: Vec<_> = MODULES.iter().map(|x| format!("load-module {x}")).collect();
  let pa =
    format!("{}\nset-default-sink trivm-speaker\nset-default-source trivm-mic", pa.join("\n"));
  let pw: Vec<_> = MODULES
    .iter()
    .map(|x| {
      format!("  {{ cmd = \"load-module\" args = \"{}\" flags = [ ] }}", x.replace('"', "\\\""))
    })
    .collect();
  let pw = format!("pulse.cmd = [\n{}\n]", pw.join("\n"));
  let load: Vec<_> = MODULES
    .iter()
    .map(|x| {
      let (module, args) = x.split_once(' ').unwrap_or((x, ""));
      format!("pactl load-module {module} {} >/dev/null", ssh::quote(args))
    })
    .collect();

  let vars = format!("PA={}\nPW={}\n", ssh::quote(&pa), ssh::quote(&pw));
  format!("{vars}{}", LINUX_INSTALL.replace("$LOAD", &load.join("\n")))
}

/// Runs a script in the guest with `data` on its stdin.
fn feed(vm: &Vm, script: &str, data: &[u8]) -> io::Result<ExitStatus> {
  let mut child = ssh::ssh(vm)?.arg("--").arg(script).stdin(Stdio::piped()).spawn()?;
  let mut stdin = child.stdin.take().ok_or_else(|| bad("ssh has no stdin"))?;
  let res = stdin.write_all(data);
  drop(stdin);
  let status = child.wait()?;
  match res {
    Err(e) if status.success() => Err(e),
    _ => Ok(status),
  }
}

// MARK: Windows

/// QEMU's D-Bus object for the sound card (its `ui/dbus-display1.xml`).
const AUDIO: (&str, &str) = ("/org/qemu/Display1/Audio", "org.qemu.Display1.Audio");

/// The most a `mic` sends the listener: an hour.
const MAX: usize = RATE as usize * 2 * 3600;

/// Hands the samples to the VM's listener (`listen`) and waits until they have played; with none,
/// only checks that the listener is there.
fn windows(vm: &Vm, pcm: Option<&[u8]>) -> io::Result<()> {
  let mut sock = UnixStream::connect(vm.state("mic.sock")).map_err(|_| {
    bad(format!("{} has no microphone yet; trivm stop {0}, then start it", vm.name))
  })?;

  let Some(pcm) = pcm else { return Ok(()) };
  if pcm.len() > MAX {
    return Err(bad("a file of more than an hour"));
  }

  sock.write_all(&(pcm.len() as u64).to_le_bytes())?;
  sock.write_all(pcm)?;
  let secs = (pcm.len() / 2) as f64 / RATE as f64;
  sock.set_read_timeout(Some(Duration::from_secs_f64(secs + 10.0)))?;
  let mut rep = Vec::new();
  sock.read_to_end(&mut rep)?;
  match rep == b"ok" {
    true => Ok(()),
    false => Err(bad(format!("{}'s microphone was cut off: another mic, or a stop", vm.name))),
  }
}

/// Runs the microphone's listener (`_mic <vm>`) beside a Windows VM's QEMU, in a session of its
/// own: it ends when QEMU does.
pub fn spawn(vm: &Vm) -> io::Result<()> {
  let log = fs::OpenOptions::new().create(true).append(true).open(vm.state("log"))?;
  let mut cmd = Command::new(env::current_exe()?);
  cmd.args(["_mic", &vm.name]).stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
  detach(&mut cmd);
  cmd.spawn()?;
  Ok(())
}

/// A file a `mic` sends, played from `start`, until it ends or that `mic` does.
struct Play {
  pcm: Vec<u8>,
  pos: usize,
  start: Instant,
  client: UnixStream,
}

/// `_mic <vm>`: the sound card's listener from the VM's start until QEMU quits, as with none QEMU
/// hands the guest its buffer unwritten. Its `Read` answered with silence, or by the clock with
/// the samples a `mic` sends on `mic.sock` (their length, then them); "ok" to it once played.
pub fn listen(name: &str) -> io::Result<()> {
  let vm = Vm::open(name)?;
  let path = vm.state("mic.sock");
  let _ = fs::remove_file(&path);
  let sock = UnixListener::bind(&path)?;
  let ino = fs::metadata(&path)?.ino();

  let (tx, rx) = mpsc::channel();
  thread::spawn(move || {
    for mut client in sock.incoming().flatten() {
      if let Ok(pcm) = request(&mut client)
        && tx.send((pcm, client)).is_err()
      {
        break;
      }
    }
  });

  let res = serve(&vm, &rx);
  // Its own only: QEMU restarted, as at the end of Windows' setup, has a listener of its own,
  // which may have bound the path before this one saw its QEMU quit.
  if fs::metadata(&path).is_ok_and(|x| x.ino() == ino) {
    let _ = fs::remove_file(&path);
  }

  res
}

/// A `mic`'s samples: their length, then them, each read in 10 seconds, so a `mic` that stops
/// sending holds up no other.
fn request(client: &mut UnixStream) -> io::Result<Vec<u8>> {
  client.set_read_timeout(Some(Duration::from_secs(10)))?;
  let mut len = [0; 8];
  client.read_exact(&mut len)?;
  let len = u64::from_le_bytes(len) as usize;
  if len > MAX {
    return Err(bad("a file of more than an hour"));
  }

  let mut pcm = vec![0; len];
  client.read_exact(&mut pcm)?;
  client.set_nonblocking(true)?;
  Ok(pcm)
}

/// Answers QEMU's calls until it quits.
fn serve(vm: &Vm, rx: &mpsc::Receiver<(Vec<u8>, UnixStream)>) -> io::Result<()> {
  let mut listener = register(vm)?;
  let (mut channels, mut play) = (0, None::<Play>);
  // The guest's mute and input level, which QEMU leaves to the listener: full until it sets them.
  let mut levels = vec![];
  loop {
    if let Ok((pcm, client)) = rx.try_recv() {
      play = Some(Play { pcm, pos: 0, start: Instant::now(), client });
    }

    // Played, or its `mic` gone: a read of 0 is its end, as it writes nothing more.
    if let Some(x) = &mut play {
      let end = Duration::from_secs_f64((x.pcm.len() / 2) as f64 / RATE as f64 + 0.5);
      if x.start.elapsed() > end {
        let _ = x.client.write_all(b"ok");
        play = None;
      } else if x.client.read(&mut [0]).is_ok_and(|n| n == 0) {
        play = None;
      }
    }

    let msg = match listener.next() {
      Ok(Some(x)) => x,
      Ok(None) => continue,
      // QEMU quit: the VM stopped.
      Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
      Err(e) => return Err(e),
    };

    match msg.member.as_str() {
      "Init" => channels = init(&msg)?,
      "Read" => {
        let mut a = msg.args();
        let (_, size) = (a.u64()?, a.u64()? as usize);
        let mut data = match &mut play {
          Some(x) => read(&x.pcm, &mut x.pos, x.start.elapsed(), size, channels),
          None => vec![0; size],
        };

        gain(&mut data, channels, &levels);
        let mut body = dbus::Buf::default();
        body.bytes(&data);
        listener.reply(&msg, "ay", &body.0)?;
      }
      "SetVolume" => {
        let mut a = msg.args();
        let (_, mute) = (a.u64()?, a.bool()?);
        levels = if mute { vec![0] } else { a.bytes()? };
        if msg.wants_reply() {
          listener.reply(&msg, "", &[])?;
        }
      }
      _ if msg.wants_reply() => listener.error(&msg, "org.freedesktop.DBus.Error.UnknownMethod")?,
      _ => {}
    }
  }
}

/// The connection of a listener registered with QEMU's display, which QEMU authenticates as its
/// server, then asks for its properties.
fn register(vm: &Vm) -> io::Result<dbus::Conn> {
  let sock = qemu::dbus_display(vm)?;
  sock.set_read_timeout(Some(Duration::from_secs(10)))?;
  let mut display = dbus::Conn::client(sock, true)?;
  let (ours, theirs) = UnixStream::pair()?;
  let mut body = dbus::Buf::default();
  body.u32(0);
  let fd = Some(theirs.as_raw_fd());
  let serial = display.call(AUDIO.0, AUDIO.1, "RegisterInListener", "h", &body.0, fd)?;
  display.wait(serial)?;
  // Kept open and read, so what QEMU sends on it doesn't pile up.
  thread::spawn(move || while display.next().is_ok() {});

  // `next` polls it once authenticated, so `serve` checks its `mic` between QEMU's calls.
  ours.set_read_timeout(Some(Duration::from_secs(10)))?;
  let listener = dbus::Conn::client(ours, false)?;
  listener.timeout(Duration::from_millis(50))?;
  Ok(listener)
}

/// The channels of the sound card's input from QEMU's `Init`, whose format must be the
/// microphone's.
fn init(msg: &dbus::Msg) -> io::Result<usize> {
  let mut a = msg.args();
  let (_, bits, signed, float) = (a.u64()?, a.u8()?, a.bool()?, a.bool()?);
  let (rate, channels) = (a.u32()?, a.u8()?);
  match (bits, signed, float, rate) {
    (16, true, false, RATE) => Ok(channels as usize),
    _ => Err(bad(format!("QEMU's microphone takes {bits}-bit samples at {rate} Hz"))),
  }
}

/// `size` bytes of frames from `pos`, the mono sample in each channel, silence past the end. What
/// no one recorded is gone, as a microphone's sound is: a read behind the clock skips it.
fn read(pcm: &[u8], pos: &mut usize, elapsed: Duration, size: usize, channels: usize) -> Vec<u8> {
  let want = size / (2 * channels.max(1));
  let now = (elapsed.as_secs_f64() * RATE as f64) as usize;
  if *pos + want + RATE as usize / 5 < now {
    *pos = now - want;
  }

  let out = (*pos..*pos + want)
    .flat_map(|i| pcm.get(i * 2..i * 2 + 2).unwrap_or(&[0, 0]).repeat(channels))
    .collect();
  *pos += want;
  out
}

/// The guest's input level on 16-bit frames, 0 to 255 a channel, the last for any more, linear
/// as QEMU's own mixer applies it for other sound cards; none given is full.
fn gain(data: &mut [u8], channels: usize, levels: &[u8]) {
  if levels.iter().all(|&x| x == u8::MAX) {
    return;
  }

  for (i, x) in data.as_chunks_mut::<2>().0.iter_mut().enumerate() {
    let level = levels.get(i % channels.max(1)).or(levels.last()).copied().unwrap_or(u8::MAX);
    let sample = i16::from_le_bytes(*x) as i32 * level as i32 / u8::MAX as i32;
    *x = (sample as i16).to_le_bytes();
  }
}

#[cfg(test)]
mod tests {
  use std::path::Path;

  use super::*;
  use crate::vm::Config;

  #[test]
  fn wav() {
    let mut wav = b"RIFF\0\0\0\0WAVE".to_vec();
    wav.extend(b"fmt \x10\0\0\0");
    wav.extend([0; 16]);
    wav.extend(b"FLLR\x03\0\0\0abc\0");
    wav.extend(b"data\x04\0\0\0\x01\x02\x03\x04");
    assert_eq!(samples(&wav).unwrap(), [1, 2, 3, 4]);
    assert!(samples(b"RIFF\0\0\0\0WAVEfmt ").is_err());
    assert!(samples(b"not a wav file").is_err());
  }

  #[test]
  fn linux() {
    let script = linux_install();
    assert!(script.contains("pactl load-module module-null-sink 'sink_name=trivm-speaker "));
    assert!(script.contains(r#"args = "module-remap-source master=trivm-mic-in.monitor "#));
    assert!(lint(&script) && lint(LINUX_HAS) && lint(MACOS_HAS) && lint(MACOS_INSTALL));
  }

  #[test]
  fn reads() {
    let pcm = [1, 0, 2, 0, 3, 0];
    let mut pos = 0;
    assert_eq!(read(&pcm, &mut pos, Duration::ZERO, 8, 2), [1, 0, 1, 0, 2, 0, 2, 0]);
    assert_eq!(read(&pcm, &mut pos, Duration::ZERO, 4, 1), [3, 0, 0, 0]);
    assert_eq!(pos, 4);
    // A second on, no one having read: the read starts at the clock.
    assert_eq!(read(&pcm, &mut pos, Duration::from_secs(1), 4, 1), [0, 0, 0, 0]);
    assert_eq!(pos, RATE as usize);

    // The guest's level a channel, then its mute; none set is full.
    let mut data = vec![0x00, 0x10, 0x00, 0xf0, 0x00, 0x10, 0x00, 0xf0];
    gain(&mut data, 2, &[]);
    assert_eq!(data, [0x00, 0x10, 0x00, 0xf0, 0x00, 0x10, 0x00, 0xf0]);
    gain(&mut data, 2, &[255, 51]);
    assert_eq!(data, [0x00, 0x10, 0xcd, 0xfc, 0x00, 0x10, 0xcd, 0xfc]);
    gain(&mut data, 2, &[0]);
    assert_eq!(data, [0; 8]);

    // The sound card's input is in the format `mic` sends.
    let config = Config { system: "windows-11".into(), cpus: 1, memory: 1, disk: 1 };
    let line = qemu::args(&Vm::at("w", config), Path::new("/q"), &[]).join(" ");
    assert!(line.contains(&format!("dbus,id=mic,in.frequency={RATE},in.channels=1")));
  }

  fn lint(script: &str) -> bool {
    let mut child = Command::new("sh").arg("-n").stdin(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
    child.wait().unwrap().success()
  }
}
