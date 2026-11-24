// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! Permissions for the app under test in a macOS guest: SIP turned off (or on) from recoveryOS,
//! whose screens trivm reads with Vision's text recognition and drives from the host, and the TCC
//! grants written for an app into the guest's databases, which root may do with SIP off.

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

/// A macOS VM that is set up: SIP and TCC are macOS's.
fn macos(name: &str) -> io::Result<Vm> {
  let vm = Vm::open(name)?;
  if vm.config.os() != Os::Macos {
    return Err(bad(format!("{name} runs Linux: SIP and TCC grants are macOS's")));
  }

  if vm.config.setup {
    return Err(bad(format!("{name} isn't set up yet; trivm start {name} finishes it")));
  }

  Ok(vm)
}

/// Whether SIP is on in the running guest, from `csrutil status`.
fn sip_on(vm: &Vm) -> io::Result<bool> {
  let out = ssh::output(vm, "csrutil status")?;
  match (out.contains("status: enabled"), out.contains("status: disabled")) {
    (true, _) => Ok(true),
    (_, true) => Ok(false),
    _ => Err(bad(format!("csrutil status in {}: {out}", vm.name))),
  }
}

// MARK: SIP

/// `sip <vm> on|off`: SIP as asked, from recoveryOS's Terminal, then the VM up again and its
/// state checked. A VM that is stopped is started first, to see if there's anything to do.
pub fn sip(name: &str, on: bool) -> io::Result<()> {
  let vm = macos(name)?;
  let word = if on { "on" } else { "off" };
  if vm.pid().is_none() {
    crate::start(name)?;
  }

  if sip_on(&vm)? == on {
    eprintln!("SIP is {word} in {name}");
    return Ok(());
  }

  eprintln!("turning SIP {word} in {name} from recoveryOS (about a minute)");
  crate::stop(&vm, false)?;
  crate::spawn(&vm, true)?;
  let shot = env::temp_dir().join(format!("trivm-{name}-recovery.png"));
  if let Err(e) = recovery(&vm, on, &shot) {
    let _ = crate::stop(&vm, true);
    return Err(bad(format!("recoveryOS: {e}; its screen: {}", shot.display())));
  }

  let _ = fs::remove_file(&shot);
  crate::start(name)?;
  if sip_on(&vm)? != on {
    return Err(bad(format!("SIP isn't {word} in {name} after recoveryOS")));
  }

  eprintln!("SIP is {word} in {name}");
  Ok(())
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
/// pixels. Vision is loaded at the first use: only `sip` reads text.
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
