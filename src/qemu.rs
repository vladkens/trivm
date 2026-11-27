// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! QEMU for Windows: the VM's command line, QEMU started as the VM's process with `swtpm` beside
//! it, and trivm's requests (`status`, `shot`, `click`, `open`, ...) done over QMP on `qmp.sock`.

use std::cell::RefCell;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use crate::display::{self, Act};
use crate::vm::{PASSWORD, Vm};
use crate::{bad, run};

const QEMU: &str = "qemu-system-aarch64";

/// The CDs attached while a VM is set up, the install media and the seed, by their device ids.
const CDS: [&str; 2] = ["cd0", "cd1"];

/// QEMU's binary and its share folder, which holds edk2's firmware.
fn qemu() -> io::Result<(PathBuf, PathBuf)> {
  let path = env::var_os("PATH").unwrap_or_default();
  let bin = env::split_paths(&path).map(|x| x.join(QEMU)).find(|x| x.is_file());
  let bin = bin.ok_or_else(|| bad(format!("{QEMU} not found; brew install qemu swtpm")))?;
  let share = bin.parent().and_then(Path::parent).map(|x| x.join("share/qemu"));
  let share = share.filter(|x| x.join("edk2-aarch64-code.fd").exists());
  let share =
    share.ok_or_else(|| bad(format!("no edk2-aarch64-code.fd beside {}", bin.display())))?;
  Ok((bin, share))
}

/// A path in QEMU's options, where a comma separates options and two stand for one.
fn opt(path: &Path) -> String {
  path.to_string_lossy().replace(',', ",,")
}

/// The VM's firmware variables: a copy of edk2's empty store.
pub fn platform(vm: &Vm) -> io::Result<()> {
  let (_, share) = qemu()?;
  run(Command::new("cp").arg("-c").arg(share.join("edk2-arm-vars.fd")).arg(vm.path("efi.bin")))?;
  Ok(())
}

/// The VM's display: virtio-gpu at its EDID's mode, drawn by virtio-win's viogpudo, which Setup
/// adds; ramfb (800×600) while Setup's media is attached, as Windows PE draws nothing on
/// virtio-gpu, and for a VM made before, with no driver for it. Both would be two monitors.
fn display(vm: &Vm, setup: bool) -> &'static str {
  match vm.gpu && !setup {
    true => "virtio-gpu-pci,xres=1920,yres=1080",
    false => "ramfb",
  }
}

/// QEMU's command line for the VM, the CDs attached as USB drives.
pub fn args(vm: &Vm, share: &Path, cds: &[PathBuf]) -> Vec<String> {
  let c = &vm.config;
  let port = vm.port.unwrap_or(0);
  let mut args: Vec<String> = [
    "-name".into(),
    vm.name.clone(),
    "-machine".into(),
    "virt".into(),
    "-accel".into(),
    "hvf".into(),
    "-cpu".into(),
    "host".into(),
    "-smp".into(),
    c.cpus.to_string(),
    "-m".into(),
    format!("{}G", c.memory),
    "-drive".into(),
    format!("if=pflash,format=raw,readonly=on,file={}", opt(&share.join("edk2-aarch64-code.fd"))),
    "-drive".into(),
    format!("if=pflash,format=raw,file={}", opt(&vm.path("efi.bin"))),
    "-drive".into(),
    format!("if=none,id=disk,format=raw,discard=unmap,file={}", opt(&vm.path("disk.img"))),
    "-device".into(),
    "nvme,drive=disk,serial=trivm,bootindex=0".into(),
    "-device".into(),
    display(vm, !cds.is_empty()).into(),
    "-device".into(),
    "qemu-xhci,id=usb".into(),
    "-device".into(),
    "usb-kbd".into(),
    "-device".into(),
    "usb-tablet".into(),
    "-netdev".into(),
    format!("user,id=net,hostfwd=tcp:127.0.0.1:{port}-:22"),
    "-device".into(),
    format!("virtio-net-pci,netdev=net,mac={}", vm.mac),
    "-chardev".into(),
    format!("socket,id=tpm,path={}", opt(&vm.state("tpm.sock"))),
    "-tpmdev".into(),
    "emulator,id=tpm,chardev=tpm".into(),
    "-device".into(),
    "tpm-tis-device,tpmdev=tpm,ppi=off".into(),
    // Windows reads the clock as local time, and its time zone is UTC.
    "-rtc".into(),
    "base=utc".into(),
    "-qmp".into(),
    format!("unix:{},server=on,wait=off", opt(&vm.state("qmp.sock"))),
    "-vnc".into(),
    "127.0.0.1:0,to=99,password=on".into(),
    "-display".into(),
    "none".into(),
  ]
  .into();

  // edk2 connects only the devices that have a boot index: the media's too, or it isn't seen. The
  // disk goes first: blank, it doesn't boot and the media does; once Setup has written Windows'
  // boot files, each of Setup's restarts boots the disk, with no key pressed.
  for (i, (cd, id)) in cds.iter().zip(CDS).enumerate() {
    let boot = if i == 0 { ",bootindex=1" } else { "" };
    args.push("-drive".into());
    args.push(format!("if=none,id={id}d,format=raw,media=cdrom,readonly=on,file={}", opt(cd)));
    args.push("-device".into());
    args.push(format!("usb-storage,drive={id}d,id={id}{boot}"));
  }

  args
}

/// A port on 127.0.0.1 that nothing listens on now.
fn free_port() -> io::Result<u16> {
  Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

unsafe extern "C" {
  fn setsid() -> i32;
  fn fcntl(fd: i32, cmd: i32, ...) -> i32;
  fn getpid() -> i32;
  fn pwrite(fd: i32, buf: *const u8, n: usize, offset: i64) -> isize;
}

const F_SETFD: i32 = 2;

/// Whether the process runs a program of this name.
fn runs(pid: i32, name: &str) -> bool {
  let out = Command::new("ps").args(["-o", "comm=", "-p", &pid.to_string()]).output();
  out.is_ok_and(|x| String::from_utf8_lossy(&x.stdout).trim_end().ends_with(name))
}

/// Waits up to a minute until the VM's process is QEMU, false if it isn't by then: a starting VM's
/// pid is trivm's until QEMU's replaces it.
pub fn started(vm: &Vm) -> bool {
  let t = Instant::now();
  while let Some(pid) = vm.pid() {
    if runs(pid, QEMU) {
      return true;
    }

    if t.elapsed() > Duration::from_secs(60) {
      return false;
    }

    thread::sleep(Duration::from_millis(200));
  }

  true
}

/// Ends a start stuck before QEMU runs: its trivm, stopped first so it makes no more processes,
/// then those it made (swtpm, QEMU forked or running) and trivm.
pub fn cancel(vm: &Vm) -> io::Result<()> {
  let Some(pid) = vm.pid() else { return Ok(()) };
  // Started meanwhile: stopped as a running VM.
  if runs(pid, QEMU) {
    return Ok(());
  }

  crate::vm::signal(pid, 17);
  let out = Command::new("pgrep").args(["-P", &pid.to_string()]).output()?;
  let out = String::from_utf8_lossy(&out.stdout);
  let mut pids: Vec<i32> = out.lines().filter_map(|x| x.trim().parse().ok()).collect();
  pids.push(pid);
  for &x in &pids {
    crate::vm::signal(x, 9);
  }

  // Gone once none of them holds the lock: a start after isn't this one.
  let t = Instant::now();
  while vm.pid().is_some_and(|x| pids.contains(&x)) {
    if t.elapsed() > Duration::from_secs(5) {
      return Err(bad(format!("{} didn't stop; its process is {pid}", vm.name)));
    }

    thread::sleep(Duration::from_millis(100));
  }

  Ok(())
}

/// Checks that the VM's folders can be given to swtpm, which splits its options at commas, with
/// no escape for one in a path, and that it has the screen ramfb draws, the firmware's 800×600.
pub fn fits(vm: &Vm) -> io::Result<()> {
  for dir in [&vm.dir, &vm.state_dir] {
    if dir.to_string_lossy().contains(',') {
      return Err(bad(format!("{}: swtpm takes no path with a comma", dir.display())));
    }
  }

  if let Some(x) = vm.config.screen {
    return Err(bad(format!("screen = {x}: Windows' screen is ramfb's here, 800x600 only")));
  }

  Ok(())
}

/// Starts `swtpm`, then QEMU as the VM's process in a session of its own, the CDs attached;
/// waits until it runs. QEMU inherits the locked `pid` file, so the VM runs exactly while QEMU
/// does.
pub fn spawn(vm: &mut Vm, cds: &[PathBuf]) -> io::Result<()> {
  fits(vm)?;
  let (bin, share) = qemu()?;
  // Held from here, so a second start finds the VM in use before touching its TPM; running once
  // its port is saved, so no command reaches the last one, which another VM may have now.
  let mut lock = vm.take()?;
  // The saved one read again: a start before this one may have saved another since it was read.
  let port = match vm.saved_port() {
    Some(x) if TcpListener::bind(("127.0.0.1", x)).is_ok() => x,
    _ => free_port()?,
  };

  vm.port = Some(port);
  vm.save_port()?;
  write!(lock, "{}", std::process::id())?;

  // Opened before swtpm starts, as a failure after leaves it running.
  let log = fs::OpenOptions::new().create(true).append(true).open(vm.state("log"))?;
  let out = log.try_clone()?;
  fs::create_dir_all(vm.path("tpm"))?;
  let _ = fs::remove_file(vm.state("tpm.sock"));
  let _ = fs::remove_file(vm.state("tpm.log"));

  let path = |x: &str| vm.state(x).to_string_lossy().into_owned();
  // swtpm's socket, listened on before swtpm runs, so QEMU's connection waits for it.
  let sock = UnixListener::bind(vm.state("tpm.sock"))?;
  let sfd = sock.as_raw_fd();
  let mut swtpm = Command::new("swtpm");
  swtpm.args(["socket", "--tpm2"]);
  swtpm.args(["--tpmstate", &format!("dir={}", vm.path("tpm").display())]);
  // Ends when QEMU's connection does.
  swtpm.args(["--ctrl", &format!("type=unixio,fd={sfd},terminate")]);
  swtpm.args(["--log", &format!("file={}", path("tpm.log"))]);
  swtpm.stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log.try_clone()?);
  // SAFETY: setsid and fcntl are async-signal-safe; the socket's fd stays open in swtpm.
  unsafe {
    swtpm.pre_exec(move || match setsid() < 0 || fcntl(sfd, F_SETFD, 0) < 0 {
      true => Err(io::Error::last_os_error()),
      false => Ok(()),
    })
  };

  // trivm's child, not a daemon, so `stop --force` finds it while the start is stuck.
  let mut tpm = swtpm.spawn()?;
  drop(sock);
  let mut tpm_kill = || {
    let _ = tpm.kill();
    let _ = tpm.wait();
  };

  let fd = lock.as_raw_fd();
  let mut cmd = Command::new(&bin);
  cmd.args(args(vm, &share, cds)).stdin(Stdio::null()).stdout(out).stderr(log);
  // SAFETY: setsid, fcntl, getpid and pwrite are async-signal-safe, the digits made on the stack;
  // the lock's fd stays open in QEMU.
  unsafe {
    cmd.pre_exec(move || {
      // The child's pid over trivm's before it runs QEMU, so a trivm killed meanwhile leaves it:
      // one write, padded and not truncated first, so a reader sees one pid or the other.
      let mut pid = [b' '; 10];
      let (mut n, mut len) = (getpid() as u32, 0);
      let mut digits = [0u8; 10];
      while len == 0 || n > 0 {
        digits[len] = b'0' + (n % 10) as u8;
        (n, len) = (n / 10, len + 1);
      }

      for i in 0..len {
        pid[i] = digits[len - 1 - i];
      }

      match setsid() < 0 || fcntl(fd, F_SETFD, 0) < 0 || pwrite(fd, pid.as_ptr(), 10, 0) != 10 {
        true => Err(io::Error::last_os_error()),
        false => Ok(()),
      }
    })
  };

  let mut child = match cmd.spawn() {
    Ok(x) => x,
    Err(e) => {
      tpm_kill();
      return Err(e);
    }
  };

  drop(lock);
  let _ = Command::new("caffeinate").args(["-i", "-w", &child.id().to_string()]).spawn();

  let t = Instant::now();
  loop {
    if let Some(code) = child.try_wait()? {
      tpm_kill();
      let log = fs::read_to_string(vm.state("log")).unwrap_or_default();
      let tpm = fs::read_to_string(vm.state("tpm.log")).unwrap_or_default();
      let tail: Vec<_> = log.lines().chain(tpm.lines()).rev().take(5).collect();
      let tail: Vec<_> = tail.into_iter().rev().collect();
      return Err(bad(format!("{} didn't start ({code}):\n{}", vm.name, tail.join("\n"))));
    }

    if ask(vm, "status", "").is_ok_and(|x| x == "running") {
      return Ok(());
    }

    if t.elapsed() > Duration::from_secs(60) {
      return Err(bad(format!("{} didn't start in a minute; see {}", vm.name, path("log"))));
    }

    thread::sleep(Duration::from_millis(200));
  }
}

// MARK: QMP

struct Qmp {
  r: BufReader<UnixStream>,
  w: UnixStream,
}

impl Qmp {
  fn connect(vm: &Vm) -> io::Result<Self> {
    let not_running = || bad(format!("{} isn't running; trivm start {}", vm.name, vm.name));
    if vm.pid().is_none() {
      return Err(not_running());
    }

    let w = UnixStream::connect(vm.state("qmp.sock")).map_err(|_| not_running())?;
    w.set_read_timeout(Some(Duration::from_secs(60)))?;
    let mut qmp = Self { r: BufReader::new(w.try_clone()?), w };
    qmp.line()?;
    qmp.call("qmp_capabilities", "{}")?;
    Ok(qmp)
  }

  fn line(&mut self) -> io::Result<String> {
    let mut line = String::new();
    if self.r.read_line(&mut line)? == 0 {
      return Err(bad("QMP closed the connection"));
    }

    Ok(line)
  }

  /// Runs a command with its arguments as a JSON object; returns the reply's line.
  fn call(&mut self, cmd: &str, args: &str) -> io::Result<String> {
    writeln!(self.w, r#"{{"execute": "{cmd}", "arguments": {args}}}"#)?;
    loop {
      let line = self.line()?;
      if line.starts_with(r#"{"return""#) {
        return Ok(line);
      }

      if line.starts_with(r#"{"error""#) {
        let desc = field(&line, "desc").unwrap_or(line.trim().to_string());
        return Err(bad(format!("{cmd}: {desc}")));
      }
    }
  }

  fn events(&mut self, events: &[String]) -> io::Result<()> {
    self.call("input-send-event", &format!(r#"{{"events": [{}]}}"#, events.join(", ")))?;
    Ok(())
  }
}

/// A JSON string.
fn json(x: &str) -> String {
  let mut s = String::from('"');
  for c in x.chars() {
    match c {
      '"' => s.push_str("\\\""),
      '\\' => s.push_str("\\\\"),
      c if (c as u32) < 0x20 => s.push_str(&format!("\\u{:04x}", c as u32)),
      c => s.push(c),
    }
  }

  s.push('"');
  s
}

/// A string field's value in a QMP reply, which has no escapes in the fields read here.
fn field(line: &str, key: &str) -> Option<String> {
  let at = line.find(&format!(r#""{key}": ""#))? + key.len() + 5;
  let end = line[at..].find('"')?;
  Some(line[at..at + end].to_string())
}

/// The VNC server's port in `query-vnc`'s reply, whose clients, listed before it, have theirs.
fn vnc_port(rep: &str) -> Option<String> {
  let Some(at) = rep.find(r#""clients": ["#) else { return field(rep, "service") };
  let end = at + rep[at..].find(']')? + 1;
  field(&format!("{}{}", &rep[..at], &rep[end..]), "service")
}

/// The screen as a PPM file's header and pixels (RGB).
struct Frame {
  width: usize,
  height: usize,
  rgb: Vec<u8>,
}

impl Frame {
  fn parse(data: &[u8]) -> io::Result<Self> {
    // "P6\n<width> <height>\n255\n" then the pixels.
    let mut fields = Vec::new();
    let mut at = 0;
    while fields.len() < 4 {
      while at < data.len() && data[at].is_ascii_whitespace() {
        at += 1;
      }

      let start = at;
      while at < data.len() && !data[at].is_ascii_whitespace() {
        at += 1;
      }

      if start == at {
        return Err(bad("the screen's dump is cut short"));
      }

      fields.push(String::from_utf8_lossy(&data[start..at]).into_owned());
    }

    let num = |x: &str| x.parse::<usize>().map_err(|_| bad("the screen's dump isn't a PPM"));
    if fields[0] != "P6" || fields[3] != "255" {
      return Err(bad("the screen's dump isn't a PPM"));
    }

    let (width, height) = (num(&fields[1])?, num(&fields[2])?);
    let rgb = data.get(at + 1..at + 1 + width * height * 3);
    let rgb = rgb.ok_or_else(|| bad("the screen's dump is cut short"))?.to_vec();
    Ok(Self { width, height, rgb })
  }

  /// The share of pixels that aren't dark, as `Display::lit` counts them.
  fn lit(&self) -> f64 {
    let px: Vec<_> = self.rgb.chunks(3).step_by(97).collect();
    let n = px.iter().filter(|x| x.iter().max().is_some_and(|x| *x as f64 / 255.0 > 0.15)).count();
    n as f64 / px.len().max(1) as f64
  }
}

/// The screen now, through a PPM dump in the VM's state folder, one per trivm process.
fn frame(vm: &Vm, qmp: &mut Qmp) -> io::Result<Frame> {
  let path = vm.state(&format!("screen-{}.ppm", std::process::id()));
  qmp.call("screendump", &format!(r#"{{"filename": {}}}"#, json(&path.to_string_lossy())))?;
  let mut data = Vec::new();
  fs::File::open(&path)?.read_to_end(&mut data)?;
  let _ = fs::remove_file(&path);
  Frame::parse(&data)
}

/// QEMU's key code for a macOS virtual key code, which `display` parses key names to.
fn qcode(code: u16) -> Option<&'static str> {
  let name = match code {
    0 => "a",
    1 => "s",
    2 => "d",
    3 => "f",
    4 => "h",
    5 => "g",
    6 => "z",
    7 => "x",
    8 => "c",
    9 => "v",
    11 => "b",
    12 => "q",
    13 => "w",
    14 => "e",
    15 => "r",
    16 => "y",
    17 => "t",
    18 => "1",
    19 => "2",
    20 => "3",
    21 => "4",
    22 => "6",
    23 => "5",
    24 => "equal",
    25 => "9",
    26 => "7",
    27 => "minus",
    28 => "8",
    29 => "0",
    30 => "bracket_right",
    31 => "o",
    32 => "u",
    33 => "bracket_left",
    34 => "i",
    35 => "p",
    36 => "ret",
    37 => "l",
    38 => "j",
    39 => "apostrophe",
    40 => "k",
    41 => "semicolon",
    42 => "backslash",
    43 => "comma",
    44 => "slash",
    45 => "n",
    46 => "m",
    47 => "dot",
    48 => "tab",
    49 => "spc",
    50 => "grave_accent",
    51 => "backspace",
    53 => "esc",
    0x37 => "meta_l",
    0x38 => "shift",
    57 => "caps_lock",
    0x3A => "alt",
    0x3B => "ctrl",
    122 => "f1",
    120 => "f2",
    99 => "f3",
    118 => "f4",
    96 => "f5",
    97 => "f6",
    98 => "f7",
    100 => "f8",
    101 => "f9",
    109 => "f10",
    103 => "f11",
    111 => "f12",
    115 => "home",
    116 => "pgup",
    117 => "delete",
    119 => "end",
    121 => "pgdn",
    123 => "left",
    124 => "right",
    125 => "down",
    126 => "up",
    _ => return None,
  };

  Some(name)
}

/// An act's events as QMP's `input-send-event` takes them; the pointer's position is the tablet's,
/// from 0 to 0x7fff across the screen.
fn events(act: &Act, width: usize, height: usize) -> io::Result<Vec<String>> {
  let abs = |x: f64, y: f64| -> io::Result<Vec<String>> {
    let (w, h) = (width as f64, height as f64);
    if x >= w || y >= h {
      return Err(bad(format!("({x}, {y}) is outside the {w}×{h} screen")));
    }

    let axis = |name: &str, v: f64, size: f64| {
      let v = ((v + 0.5) * 32768.0 / size).min(32767.0) as u32;
      format!(r#"{{"type": "abs", "data": {{"axis": "{name}", "value": {v}}}}}"#)
    };
    Ok(vec![axis("x", x, w), axis("y", y, h)])
  };

  match *act {
    Act::Move(x, y) => abs(x, y),
    Act::Button { x, y, right, down, .. } => {
      let mut events = abs(x, y)?;
      let button = if right { "right" } else { "left" };
      events
        .push(format!(r#"{{"type": "btn", "data": {{"down": {down}, "button": "{button}"}}}}"#));
      Ok(events)
    }
    Act::Key(code, down) => {
      let key = qcode(code).ok_or_else(|| bad(format!("no QEMU key for key code {code}")))?;
      Ok(vec![format!(
        r#"{{"type": "key", "data": {{"down": {down}, "key": {{"type": "qcode", "data": "{key}"}}}}}}"#
      )])
    }
  }
}

/// A request as `vz::ask` takes it, done over QMP.
pub fn ask(vm: &Vm, req: &str, body: &str) -> io::Result<String> {
  let mut qmp = Qmp::connect(vm)?;
  let words: Vec<&str> = req.split(' ').collect();
  let num = |x: &str| x.parse::<f64>().map_err(|_| bad(format!("not a number: {x}")));

  // The pointer's events need the screen's size, the keys' don't.
  let input = |mut qmp: Qmp, sized: bool| -> io::Result<(RefCell<Qmp>, usize, usize)> {
    let (w, h) = match sized {
      true => frame(vm, &mut qmp).map(|x| (x.width, x.height))?,
      false => (0, 0),
    };

    Ok((RefCell::new(qmp), w, h))
  };

  match words[..] {
    ["status"] => {
      let rep = qmp.call("query-status", "{}")?;
      Ok(field(&rep, "status").unwrap_or_default())
    }
    ["stop"] => qmp.call("system_powerdown", "{}").map(|_| String::new()),
    ["kill"] => qmp.call("quit", "{}").map(|_| String::new()),
    // The screen's own pixels, a point each.
    ["shot"] | ["shot", "full"] => {
      let args = format!(r#"{{"filename": {}, "format": "png"}}"#, json(body));
      qmp.call("screendump", &args).map(|_| String::new())
    }
    ["lit"] => Ok(format!("{:.2}", frame(vm, &mut qmp)?.lit())),
    ["open"] => {
      let args = format!(r#"{{"protocol": "vnc", "password": {}}}"#, json(PASSWORD));
      qmp.call("set_password", &args)?;
      let rep = qmp.call("query-vnc", "{}")?;
      let port = vnc_port(&rep).ok_or_else(|| bad("QEMU's VNC isn't on"))?;
      run(Command::new("open").arg(format!("vnc://:{PASSWORD}@127.0.0.1:{port}")))?;
      Ok(String::new())
    }

    ["move", x, y] | ["click", x, y, ..] => {
      let (qmp, w, h) = input(qmp, true)?;
      let act = |a: Act| qmp.borrow_mut().events(&events(&a, w, h)?);
      match words[..] {
        ["click", _, _, button, clicks] => {
          let clicks = clicks.parse().map_err(|_| bad("not a click count"))?;
          display::pointer(num(x)?, num(y)?, Some(button == "right"), clicks, &act)
        }
        _ => display::pointer(num(x)?, num(y)?, None, 0, &act),
      }
    }
    ["type"] | ["key", _] => {
      let (qmp, _, _) = input(qmp, false)?;
      let act = |a: Act| qmp.borrow_mut().events(&events(&a, 0, 0)?);
      match words[..] {
        ["key", keys] => display::keys(keys, &act),
        _ => display::typing(body, &act),
      }
    }
    _ => Err(bad(format!("unknown request: {req}"))),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::vm::{Config, Screen};

  #[test]
  fn command_line() {
    let config = Config { system: "windows-11".into(), cpus: 4, memory: 6, disk: 64, screen: None };
    let (dir, state_dir) = ("/v/a,b".into(), "/s/a,b".into());
    let mac = "02:00:00:00:00:01".into();
    let vm = Vm { dir, state_dir, mac, setup: true, port: Some(50022), ..Vm::at("w", config) };
    let cds = ["/c/win.iso".into(), "/v/a,b/seed.iso".into()];
    let line = args(&vm, Path::new("/q"), &cds).join(" ");

    for x in [
      "-machine virt -accel hvf -cpu host -smp 4 -m 6G",
      "if=pflash,format=raw,readonly=on,file=/q/edk2-aarch64-code.fd",
      "if=pflash,format=raw,file=/v/a,,b/efi.bin",
      "file=/v/a,,b/disk.img -device nvme,drive=disk,serial=trivm,bootindex=0 ",
      "-device ramfb -device qemu-xhci,id=usb -device usb-kbd -device usb-tablet",
      "user,id=net,hostfwd=tcp:127.0.0.1:50022-:22",
      "virtio-net-pci,netdev=net,mac=02:00:00:00:00:01",
      "socket,id=tpm,path=/s/a,,b/tpm.sock",
      "tpm-tis-device,tpmdev=tpm,ppi=off -rtc base=utc",
      "-qmp unix:/s/a,,b/qmp.sock,server=on,wait=off",
      "-vnc 127.0.0.1:0,to=99,password=on -display none",
      "media=cdrom,readonly=on,file=/c/win.iso -device usb-storage,drive=cd0d,id=cd0,bootindex=1",
      "file=/v/a,,b/seed.iso -device usb-storage,drive=cd1d,id=cd1",
    ] {
      assert!(line.contains(x), "{x} not in {line}");
    }

    assert!(!args(&vm, Path::new("/q"), &[]).join(" ").contains("usb-storage"));

    // A VM with viogpudo draws on virtio-gpu once set up, on ramfb for Setup; never on both.
    let vm = Vm { gpu: true, ..vm };
    let gpu = args(&vm, Path::new("/q"), &[]).join(" ");
    assert!(gpu.contains("=0 -device virtio-gpu-pci,xres=1920,yres=1080 -device qemu-xhci"));
    assert!(!gpu.contains("ramfb"));
    let setup = args(&vm, Path::new("/q"), &cds).join(" ");
    assert!(setup.contains("-device ramfb -device") && !setup.contains("virtio-gpu"));

    // QEMU's paths take a comma doubled; swtpm's none.
    assert!(fits(&vm).is_err());
    let vm = Vm { dir: "/v/a".into(), state_dir: "/s/a".into(), ..vm };
    assert!(fits(&vm).is_ok());
    // Its screen is ramfb's.
    let screen = Some(Screen { width: 1280.0, height: 800.0, scale: 1.0 });
    let config = Config { screen, ..vm.config.clone() };
    assert!(fits(&Vm { config, ..vm }).is_err());
  }

  #[test]
  fn replies() {
    let rep = r#"{"return": {"status": "running", "singlestep": false, "running": true}}"#;
    assert_eq!(field(rep, "status").as_deref(), Some("running"));
    assert_eq!(field(rep, "nope"), None);
    assert_eq!(json("a\"b\\c\n"), r#""a\"b\\c\u000a""#);

    // QEMU 11.1.3's order: the clients before the server's own fields.
    let vnc = r#"{"return": {"enabled": true, "auth": "vnc", "family": "ipv4", "clients": [CLIENTS], "service": "5901", "host": "127.0.0.1"}}"#;
    let client =
      r#"{"host": "127.0.0.1", "service": "54321", "family": "ipv4", "websocket": false}"#;
    assert_eq!(vnc_port(&vnc.replace("CLIENTS", "")).as_deref(), Some("5901"));
    assert_eq!(vnc_port(&vnc.replace("CLIENTS", client)).as_deref(), Some("5901"));
    assert_eq!(vnc_port(r#"{"return": {"enabled": false}}"#), None);
  }

  #[test]
  fn frames() {
    let mut data = b"P6\n4 2\n255\n".to_vec();
    data.extend([0u8; 12]);
    data.extend([255u8; 12]);
    let f = Frame::parse(&data).unwrap();
    assert_eq!((f.width, f.height, f.rgb.len()), (4, 2, 24));
    assert!(Frame::parse(&data[..20]).is_err());
    assert!(Frame::parse(b"P5\n1 1\n255\n\0").is_err());
  }

  #[test]
  fn inputs() {
    let e = events(&Act::Move(0.0, 767.0), 1024, 768).unwrap();
    assert_eq!(e[0], r#"{"type": "abs", "data": {"axis": "x", "value": 16}}"#);
    assert_eq!(e[1], r#"{"type": "abs", "data": {"axis": "y", "value": 32746}}"#);
    assert!(events(&Act::Move(1024.0, 0.0), 1024, 768).is_err());

    let b = Act::Button { x: 1.0, y: 1.0, right: true, down: false, clicks: 1 };
    assert!(events(&b, 10, 10).unwrap()[2].contains(r#""down": false, "button": "right""#));
    let k = events(&Act::Key(0x37, true), 0, 0).unwrap();
    assert!(k[0].contains(r#""data": "meta_l""#));

    // Every key `display` can name or type has a QEMU key.
    for c in (' '..='~').chain(['\n', '\t']) {
      assert!(display::chord(c).unwrap().iter().all(|x| qcode(*x).is_some()), "{c:?}");
    }

    for x in ["enter", "esc", "win", "ctrl", "alt", "f4", "delete", "pagedown", "up", "capslock"] {
      assert!(display::combo(x).unwrap().iter().all(|x| qcode(*x).is_some()), "{x}");
    }
  }
}
