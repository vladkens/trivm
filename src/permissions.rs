// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! `os grant` and `os sip`, permissions for the app under test in a macOS guest: the TCC grants
//! written for an app into the guest's databases, which root may do with SIP off, and SIP turned
//! off (or on) from recoveryOS, whose screens trivm reads with Vision's text recognition and drives
//! from the host.

use std::ffi::{CStr, c_char, c_void};
use std::io::{self, Read};
use std::path::Path;
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use objc2::msg_send;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSRange, NSRect, NSString};

use crate::vm::{Os, PASSWORD, Vm};
use crate::{bad, ssh, vz};

/// A macOS VM that is set up, for the `os` command `cmd`: SIP and TCC are macOS's.
fn macos(name: &str, cmd: &str) -> io::Result<Vm> {
  let vm = Vm::open(name)?;
  if vm.config.os() != Os::Macos {
    return Err(bad(format!("os {cmd}: not supported on {}", vm.config.system)));
  }

  if vm.setup {
    return Err(bad(format!("{name} isn't set up yet; trivm start {name} finishes it")));
  }

  Ok(vm)
}

/// Whether SIP is on in the running guest, from `csrutil status`; asked up to three times, as a
/// guest just booted (load 45) once dropped the SSH session, exit 255 with no message.
fn sip_on(vm: &Vm) -> io::Result<bool> {
  let mut tries = 1;
  let out = loop {
    match ssh::output(vm, "csrutil status") {
      Err(_) if tries < 3 => tries += 1,
      x => break x?,
    }

    thread::sleep(Duration::from_secs(3));
  };

  match (out.contains("status: enabled"), out.contains("status: disabled")) {
    (true, _) => Ok(true),
    (_, true) => Ok(false),
    _ => Err(bad(format!("csrutil status in {}: {out}", vm.name))),
  }
}

// MARK: SIP

/// `os sip <vm> on|off`: SIP as asked. A VM that is stopped is started first, to see if there's
/// anything to do; it is up after.
pub fn sip(name: &str, on: bool) -> io::Result<()> {
  let vm = macos(name, "sip")?;
  let word = word(on);
  if vm.pid().is_none() {
    crate::start(name)?;
  }

  if sip_on(&vm)? != on {
    eprintln!("turning SIP {word} in {name} from recoveryOS (about a minute)");
    turn(&vm, on)?;
  }

  eprintln!("SIP is {word} in {name}");
  Ok(())
}

fn word(on: bool) -> &'static str {
  if on { "on" } else { "off" }
}

/// SIP turned on or off from recoveryOS's Terminal, then the VM up again and its state checked.
fn turn(vm: &Vm, on: bool) -> io::Result<()> {
  crate::stop(vm, false)?;
  crate::spawn(vm, true)?;
  let shot = env::temp_dir().join(format!("trivm-{}-recovery.png", vm.name));
  if let Err(e) = recovery(vm, on, &shot) {
    let _ = crate::stop(vm, true);
    return Err(bad(format!("recoveryOS: {e}; its screen: {}", shot.display())));
  }

  let _ = fs::remove_file(&shot);
  crate::start(&vm.name)?;
  match sip_on(vm)? == on {
    true => Ok(()),
    false => Err(bad(format!("SIP isn't {} in {} after recoveryOS", word(on), vm.name))),
  }
}

/// Drives recoveryOS of macOS 27 from its startup options to `csrutil` in its Terminal, which
/// asks to confirm and for the password of the only admin, then halts it. Each step waits for its
/// screen's text; the last screen read is `shot`.
fn recovery(vm: &Vm, on: bool, shot: &Path) -> io::Result<()> {
  let click = |(x, y): (f64, f64)| vz::ask(vm, &format!("click {x} {y} left 1"), "");
  let typing = |text: &str| vz::ask(vm, "type", text);
  let screen = |text: &str, secs: u64| wait_text(vm, shot, text, Duration::from_secs(secs));

  // The startup options: the disk and Options, which shows its Continue button once chosen.
  click(screen("Options", 180)?)?;
  click(screen("Continue", 30)?)?;
  // Recovery's menu bar, then Utilities › Terminal by its shortcut.
  screen("Utilities", 300)?;
  vz::ask(vm, "key cmd+shift+t", "")?;
  screen("bash", 60)?;
  typing(&format!("csrutil {}\n", if on { "enable" } else { "disable" }))?;
  screen("y/n", 60)?;
  typing("y\n")?;
  screen("password", 60)?;
  typing(&format!("{PASSWORD}\n"))?;
  screen(if on { "Protection is on" } else { "Protection is off" }, 120)?;
  typing("halt\n")?;

  let t = Instant::now();
  while vm.pid().is_some() {
    if t.elapsed() > Duration::from_secs(60) {
      return Err(bad("didn't halt in a minute"));
    }

    thread::sleep(Duration::from_millis(500));
  }

  Ok(())
}

/// Waits until `text` shows on the VM's screen; the middle of it, in a shot's pixels.
fn wait_text(vm: &Vm, shot: &Path, text: &str, timeout: Duration) -> io::Result<(f64, f64)> {
  let t = Instant::now();
  loop {
    if vm.pid().is_none() {
      return Err(bad(format!("{} stopped", vm.name)));
    }

    // A screen with no frame yet is a screen without the text.
    if vz::ask(vm, "shot", &shot.to_string_lossy()).is_ok()
      && let Some(x) = find_text(shot, text)?
    {
      return Ok(x);
    }

    if t.elapsed() > timeout {
      return Err(bad(format!("no \"{text}\" on the screen in {} s", timeout.as_secs())));
    }

    thread::sleep(Duration::from_secs(1));
  }
}

unsafe extern "C" {
  fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
}

/// Where `want` shows in a PNG, by Vision's text recognition: the middle of it, in the PNG's
/// pixels. Vision is loaded at the first use: only recoveryOS's steps read text.
pub fn find_text(png: &Path, want: &str) -> io::Result<Option<(f64, f64)>> {
  let lib = c"/System/Library/Frameworks/Vision.framework/Vision";
  // SAFETY: a system framework, whose initializers are Apple's; RTLD_LAZY.
  if unsafe { dlopen(lib.as_ptr(), 1) }.is_null() {
    return Err(bad("Vision.framework can't be loaded"));
  }

  let class = |x: &CStr| AnyClass::get(x).ok_or_else(|| bad(format!("no {x:?} in Vision")));
  let (w, h) = png_size(png)?;

  // SAFETY: the methods and types of Vision's VNRecognizeTextRequest and VNImageRequestHandler.
  unsafe {
    // Both levels, accurate (0) and fast (1): accurate read nothing at all in a shot of
    // recoveryOS's Terminal just opened, where fast read its title.
    let mut requests = Vec::new();
    for level in [0isize, 1] {
      let request: Retained<AnyObject> = msg_send![class(c"VNRecognizeTextRequest")?, new];
      let _: () = msg_send![&*request, setRecognitionLevel: level];
      let _: () = msg_send![&*request, setUsesLanguageCorrection: false];
      requests.push(request);
    }

    let handler: Allocated<AnyObject> = msg_send![class(c"VNImageRequestHandler")?, alloc];
    let none = NSDictionary::<NSString, AnyObject>::new();
    let handler: Retained<AnyObject> =
      msg_send![handler, initWithURL: &*vz::url(png), options: &*none];
    let all = NSArray::from_retained_slice(&requests);
    let res: Result<(), Retained<NSError>> = msg_send![&*handler, performRequests: &*all, error: _];
    res.map_err(|e| bad(format!("reading text: {}", e.localizedDescription())))?;

    for request in &requests {
      let results: Option<Retained<NSArray<AnyObject>>> = msg_send![&**request, results];
      for line in results.iter().flat_map(|x| x.iter()) {
        let top: Retained<NSArray<AnyObject>> = msg_send![&*line, topCandidates: 1usize];
        let Some(text) = top.firstObject() else { continue };
        let s: Retained<NSString> = msg_send![&*text, string];
        let s = s.to_string();
        let Some(at) = s.find(want) else { continue };

        // A line may hold more than the text: its own box, normalized, from the bottom left.
        let range = NSRange::new(s[..at].encode_utf16().count(), want.encode_utf16().count());
        let part: Result<Retained<AnyObject>, Retained<NSError>> =
          msg_send![&*text, boundingBoxForRange: range, error: _];
        let r: NSRect = match part {
          Ok(x) => msg_send![&*x, boundingBox],
          Err(_) => msg_send![&*line, boundingBox],
        };

        let x = (r.origin.x + r.size.width / 2.0) * w;
        let y = (1.0 - r.origin.y - r.size.height / 2.0) * h;
        return Ok(Some((x.round(), y.round())));
      }
    }
  }

  Ok(None)
}

/// A PNG's width and height, from its header.
fn png_size(png: &Path) -> io::Result<(f64, f64)> {
  let mut b = [0u8; 24];
  fs::File::open(png)?.read_exact(&mut b)?;
  if &b[..8] != b"\x89PNG\r\n\x1a\n" {
    return Err(bad(format!("{}: not a PNG", png.display())));
  }

  let num = |i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as f64;
  Ok((num(16), num(20)))
}

// MARK: TCC grants

/// A service `os grant` takes: its name, TCC's services for it, and whether the user's database
/// keeps them, not the system's.
struct Service {
  name: &'static str,
  tcc: &'static [&'static str],
  user: bool,
}

const SERVICES: &[Service] = &[
  // Sending events (CGEventPost) is a service of its own beside Accessibility.
  Service {
    name: "accessibility",
    tcc: &["kTCCServiceAccessibility", "kTCCServicePostEvent"],
    user: false,
  },
  Service { name: "screen", tcc: &["kTCCServiceScreenCapture"], user: false },
  Service { name: "input", tcc: &["kTCCServiceListenEvent"], user: false },
  Service { name: "microphone", tcc: &["kTCCServiceMicrophone"], user: true },
];

/// Prints the app's TCC client: a bundle by its identifier (type 0), which is what tccd looks a
/// bundle's executable up by, anything else by its path (type 1) with its links resolved, as tccd
/// sees the process.
const APP: &str = r#"set -e
if [ -f "$1/Contents/Info.plist" ]; then
  echo 0
  /usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$1/Contents/Info.plist"
elif [ -f "$1" ]; then
  echo 1
  realpath "$1"
else
  echo "no app at $1" >&2
  exit 1
fi
"#;

/// Runs `$1` on the system's TCC.db and `$2`, if any, on the user's (macOS 27's is in a container
/// of tccd's, older ones' in `~/Library`), then kills tccd if it runs, which reads them once and
/// ignores SIGTERM; launchd starts it at the next request.
const WRITE: &str = r#"set -e
printf '%s' "$1" | sudo -n sqlite3 '/Library/Application Support/com.apple.TCC/TCC.db'
if [ -n "$2" ]; then
  db="$HOME/Library/Application Support/com.apple.TCC/TCC.db"
  for x in /private/var/containers/Data/ProtectedSystem/*/; do
    m="$x.com.apple.containermanagerd.metadata.plist"
    id=$(sudo -n plutil -extract MCMMetadataIdentifier raw "$m" 2>/dev/null || true)
    uid=$(sudo -n plutil -extract MCMMetadataOwnership.uid raw "$m" 2>/dev/null || true)
    if [ "$id" = com.apple.tccd ] && [ "$uid" = "$(id -u)" ]; then
      db="${x}Data/Library/Application Support/com.apple.TCC/TCC.db"
    fi
  done
  printf '%s' "$2" | sudo -n sqlite3 "$db"
fi
sudo -n pkill -9 -x tccd || [ $? -eq 1 ]
"#;

/// `os grant <vm> <app> [<service>...]`: the services, all by default, allowed for the app at a
/// path in the guest. A VM that is stopped is started first and is up after; SIP, which keeps
/// TCC's databases from root, is turned off first if it's on, once the app is found.
pub fn grant(name: &str, app: &str, names: &[String]) -> io::Result<()> {
  let vm = macos(name, "grant")?;
  let services = services(names)?;
  if !app.starts_with('/') {
    return Err(bad(format!("{app}: the app's path in the guest, from /")));
  }

  if vm.pid().is_none() {
    crate::start(name)?;
  }

  // Asked first: it waits out a guest just booted that drops the SSH session.
  let sip = sip_on(&vm)?;
  let out = ssh::output(&vm, &format!("sh -c {} app {}", ssh::quote(APP), ssh::quote(app)))?;
  let lines: Vec<&str> = out.lines().collect();
  let [kind, client] = lines[..] else {
    return Err(bad(format!("{app} in {name}: {out}")));
  };

  if sip {
    eprintln!("SIP is on in {name}: turning it off from recoveryOS first (about a minute)");
    turn(&vm, false)?;
  }

  let kind: u8 = kind.parse().map_err(|_| bad(format!("{app} in {name}: {out}")))?;
  let pick = |user: bool| -> Vec<&str> {
    services.iter().filter(|s| s.user == user).flat_map(|s| s.tcc.iter().copied()).collect()
  };
  let (system, user) = (sql(&pick(false), client, kind), sql(&pick(true), client, kind));
  let script =
    format!("sh -c {} write {} {}", ssh::quote(WRITE), ssh::quote(&system), ssh::quote(&user));
  ssh::output(&vm, &script)?;

  let names: Vec<_> = services.iter().map(|s| s.name).collect();
  eprintln!(
    "{client} may use {} in {name}; an app that runs sees them once relaunched",
    names.join(", ")
  );
  Ok(())
}

/// The services named, all with none.
fn services(names: &[String]) -> io::Result<Vec<&'static Service>> {
  let all: Vec<_> = SERVICES.iter().map(|s| s.name).collect();
  let find = |x: &String| {
    let known = SERVICES.iter().find(|s| s.name == x);
    known.ok_or_else(|| bad(format!("unknown service {x}; {}", all.join(", "))))
  };
  match names {
    [] => Ok(SERVICES.iter().collect()),
    _ => names.iter().map(find).collect(),
  }
}

/// Rows that allow `client` the services, with no code requirement (`csreq`): tccd honours them
/// on macOS 27, and they hold for every build of the app, where an ad hoc signed app's designated
/// requirement is its hash.
fn sql(services: &[&str], client: &str, kind: u8) -> String {
  let client = client.replace('\'', "''");
  let row = |s: &&str| {
    format!(
      "INSERT OR REPLACE INTO access (service, client, client_type, auth_value, auth_reason, \
      auth_version, csreq, flags) VALUES ('{s}', '{client}', {kind}, 2, 4, 1, NULL, 0);\n"
    )
  };
  services.iter().map(row).collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn service_names() {
    let names = |x: &[&str]| -> io::Result<Vec<&str>> {
      let x: Vec<String> = x.iter().map(|x| x.to_string()).collect();
      Ok(services(&x)?.iter().map(|s| s.name).collect())
    };
    assert_eq!(names(&[]).unwrap(), ["accessibility", "screen", "input", "microphone"]);
    assert_eq!(names(&["microphone", "screen"]).unwrap(), ["microphone", "screen"]);
    assert!(names(&["camera"]).is_err());
  }

  #[test]
  fn rows() {
    assert_eq!(sql(&[], "a", 0), "");
    assert_eq!(
      sql(&["kTCCServiceAccessibility", "kTCCServicePostEvent"], "me.vladkens.wavo", 0),
      "INSERT OR REPLACE INTO access (service, client, client_type, auth_value, auth_reason, \
      auth_version, csreq, flags) VALUES ('kTCCServiceAccessibility', 'me.vladkens.wavo', 0, 2, 4, \
      1, NULL, 0);\nINSERT OR REPLACE INTO access (service, client, client_type, auth_value, \
      auth_reason, auth_version, csreq, flags) VALUES ('kTCCServicePostEvent', 'me.vladkens.wavo', \
      0, 2, 4, 1, NULL, 0);\n"
    );
    assert!(sql(&["kTCCServiceMicrophone"], "/opt/it's/app", 1).contains("'/opt/it''s/app', 1,"));
  }
}
