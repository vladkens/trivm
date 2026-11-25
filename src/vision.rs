// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! The text on a macOS guest's screen, read from shots by Vision's text recognition, for the
//! screens trivm answers itself: recoveryOS for `sip`, the Setup Assistant of macOS 13 and 14,
//! the shut-down dialog.

use std::ffi::{CStr, c_char, c_void};
use std::io::{self, Read};
use std::path::Path;
use std::time::{Duration, Instant};
use std::{fs, thread};

use objc2::msg_send;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSRange, NSRect, NSString};

use crate::vm::Vm;
use crate::{bad, vz};

/// A line of text read in a shot, and its middle in the shot's pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
  pub text: String,
  pub x: f64,
  pub y: f64,
}

/// Waits until `text` shows on the VM's screen; the middle of it, in a shot's pixels.
pub fn wait_text(vm: &Vm, shot: &Path, text: &str, timeout: Duration) -> io::Result<(f64, f64)> {
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

/// Where `want` shows in a PNG, in any line: the middle of it, in the PNG's pixels.
pub fn find_text(png: &Path, want: &str) -> io::Result<Option<(f64, f64)>> {
  let (w, h) = png_size(png)?;
  for Found { line, text, string: s } in recognize(png)? {
    let Some(at) = s.find(want) else { continue };
    // A line may hold more than the text: its own box, normalized, from the bottom left.
    let range = NSRange::new(s[..at].encode_utf16().count(), want.encode_utf16().count());
    // SAFETY: the methods of VNRecognizedText and VNRectangleObservation.
    let r: NSRect = unsafe {
      let part: Result<Retained<AnyObject>, Retained<NSError>> =
        msg_send![&*text, boundingBoxForRange: range, error: _];
      match part {
        Ok(x) => msg_send![&*x, boundingBox],
        Err(_) => msg_send![&*line, boundingBox],
      }
    };

    return Ok(Some(middle(r, w, h)));
  }

  Ok(None)
}

/// The lines of text in a PNG, top to bottom; a line read at both levels shows twice.
pub fn lines(png: &Path) -> io::Result<Vec<Line>> {
  let (w, h) = png_size(png)?;
  let mut out = Vec::new();
  for Found { line, string: text, .. } in recognize(png)? {
    // SAFETY: VNRectangleObservation's box.
    let r: NSRect = unsafe { msg_send![&*line, boundingBox] };
    let (x, y) = middle(r, w, h);
    out.push(Line { text, x, y });
  }

  out.sort_by(|a, b| a.y.total_cmp(&b.y));
  Ok(out)
}

/// A normalized box from the bottom left, as Vision gives it: its middle in pixels.
fn middle(r: NSRect, w: f64, h: f64) -> (f64, f64) {
  let x = (r.origin.x + r.size.width / 2.0) * w;
  let y = (1.0 - r.origin.y - r.size.height / 2.0) * h;
  (x.round(), y.round())
}

unsafe extern "C" {
  fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
}

/// A line Vision read: its observation, the best candidate for its text, and that one's string.
struct Found {
  line: Retained<AnyObject>,
  text: Retained<AnyObject>,
  string: String,
}

/// Vision's lines in a PNG. Vision is loaded at the first use: only these screens need it.
fn recognize(png: &Path) -> io::Result<Vec<Found>> {
  let lib = c"/System/Library/Frameworks/Vision.framework/Vision";
  // SAFETY: a system framework, whose initializers are Apple's; RTLD_LAZY.
  if unsafe { dlopen(lib.as_ptr(), 1) }.is_null() {
    return Err(bad("Vision.framework can't be loaded"));
  }

  let class = |x: &CStr| AnyClass::get(x).ok_or_else(|| bad(format!("no {x:?} in Vision")));
  let mut out = Vec::new();
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
        out.push(Found { line, text, string: s.to_string() });
      }
    }
  }

  Ok(out)
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
