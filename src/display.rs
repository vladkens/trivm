// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! The guest's screen from the host: a VZVirtualMachineView kept on screen in a tiny window,
//! shots read from the IOSurface its layers show, clicks and keys sent to it as NSEvents, which
//! VZ hands to the guest's pointer and keyboard; `open` moves the view into a normal window.
//!
//! The tiny window is Vitrine's `DisplaySink` (Apache-2.0,
//! https://github.com/vitrine-vm/vitrine/blob/main/Sources/VitrineVZ/DisplaySink.swift): a view
//! presents the display only while it is in a window that is composited, so the window is opaque,
//! 4×4 points, at desktop level + 1, below every app's window.

use std::path::Path;
use std::time::Duration;
use std::{fs, io, thread};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{AllocAnyThread, MainThreadMarker, MainThreadOnly, msg_send};
use objc2_app_kit::{
  NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSBitmapImageFileType,
  NSBitmapImageRep, NSColorSpace, NSEvent, NSEventModifierFlags, NSEventType, NSScreen, NSView,
  NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::CGAffineTransform;
use objc2_core_graphics::{CGEvent, CGEventField, CGWindowLevelForKey, CGWindowLevelKey};
use objc2_core_image::CIImage;
use objc2_foundation::{NSDictionary, NSPoint, NSProcessInfo, NSRect, NSSize, NSString};
use objc2_io_surface::IOSurfaceRef;
use objc2_quartz_core::CALayer;
use objc2_virtualization::{VZVirtualMachine, VZVirtualMachineView};

use crate::bad;

/// A shot's size, and the display's pixels per shot pixel.
#[derive(Clone, Copy)]
pub struct Screen {
  pub width: f64,
  pub height: f64,
  pub scale: f64,
}

/// One input event, done on the main thread; the waits between them are the caller's.
pub enum Act {
  Move(f64, f64),
  Button { x: f64, y: f64, right: bool, down: bool, clicks: isize },
  Key(u16, bool),
}

pub struct Display {
  view: Retained<VZVirtualMachineView>,
  /// The tiny window's content, which holds the view while no window is open.
  holder: Retained<NSView>,
  window: Option<Retained<NSWindow>>,
  screen: Screen,
  title: String,
  /// Modifiers held by the keys sent so far.
  flags: NSEventModifierFlags,
  _sink: Retained<NSWindow>,
}

impl Display {
  pub fn new(
    mtm: MainThreadMarker,
    machine: &VZVirtualMachine,
    screen: Screen,
    title: &str,
  ) -> Self {
    let origin =
      NSScreen::mainScreen(mtm).map(|x| x.frame().origin).unwrap_or(NSPoint::new(0.0, 0.0));
    let tiny = NSSize::new(4.0, 4.0);
    // SAFETY: a window made and used on the main thread, kept by `Display`.
    let sink = unsafe {
      NSWindow::initWithContentRect_styleMask_backing_defer(
        NSWindow::alloc(mtm),
        NSRect::new(origin, tiny),
        NSWindowStyleMask::Borderless,
        NSBackingStoreType::Buffered,
        false,
      )
    };

    unsafe { sink.setReleasedWhenClosed(false) };
    sink.setIgnoresMouseEvents(true);
    sink.setHasShadow(false);
    sink.setLevel(CGWindowLevelForKey(CGWindowLevelKey::DesktopWindowLevelKey) as isize + 1);
    sink.setCollectionBehavior(
      NSWindowCollectionBehavior::Stationary
        | NSWindowCollectionBehavior::IgnoresCycle
        | NSWindowCollectionBehavior::CanJoinAllSpaces,
    );

    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(screen.width, screen.height));
    // SAFETY: the view shows the VM, both alive while `Display` is.
    let view = unsafe {
      let view = VZVirtualMachineView::initWithFrame(VZVirtualMachineView::alloc(mtm), frame);
      view.setVirtualMachine(Some(machine));
      view.setAutomaticallyReconfiguresDisplay(false);
      view.setCapturesSystemKeys(false);
      view
    };

    let holder =
      NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), tiny));
    holder.addSubview(&view);
    sink.setContentView(Some(&holder));
    sink.orderFrontRegardless();

    let flags = NSEventModifierFlags::empty();
    Self { view, holder, window: None, screen, title: title.into(), flags, _sink: sink }
  }

  /// The screen as a bitmap, scaled by `k`, opaque: Linux's framebuffer is XRGB, its unused byte
  /// zero, which an image reads as alpha.
  fn image(&self, k: f64) -> io::Result<Retained<NSBitmapImageRep>> {
    let surface = surface(&self.view).ok_or_else(|| bad("the screen has no frame yet"))?;
    let t = CGAffineTransform { a: k, b: 0.0, c: 0.0, d: k, tx: 0.0, ty: 0.0 };
    // SAFETY: the surface is an IOSurface (checked by `surface`), kept alive by the layer.
    let image = unsafe {
      let surface = &*(Retained::as_ptr(&surface) as *const IOSurfaceRef);
      let image = CIImage::imageWithIOSurface(surface);
      image.imageBySettingAlphaOneInExtent(image.extent()).imageByApplyingTransform(t)
    };

    Ok(NSBitmapImageRep::initWithCIImage(NSBitmapImageRep::alloc(), &image))
  }

  /// Writes a PNG of the screen at the shot's size; returns its size in pixels.
  pub fn shot(&self, path: &Path) -> io::Result<(isize, isize)> {
    let rep = self.image(1.0 / self.screen.scale)?;
    let none = NSDictionary::new();
    // SAFETY: no properties, which the method takes as a dictionary of property keys to objects.
    let png = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &none) };
    let png = png.ok_or_else(|| bad("the shot can't be written as PNG"))?;
    fs::write(path, png.to_vec())?;
    Ok((rep.pixelsWide(), rep.pixelsHigh()))
  }

  /// The share of the screen that isn't dark, from 0 to 1: a system starting up shows a black
  /// screen with a logo or text, a desktop is lit.
  pub fn lit(&self) -> io::Result<f64> {
    let rep = self.image(64.0 / (self.screen.width * self.screen.scale))?;
    let rgb = NSColorSpace::deviceRGBColorSpace();
    let (w, h) = (rep.pixelsWide(), rep.pixelsHigh());
    let bright = |(x, y): (isize, isize)| {
      let color = rep.colorAtX_y(x, y).and_then(|x| x.colorUsingColorSpace(&rgb));
      color.is_some_and(|x| x.brightnessComponent() > 0.15)
    };

    let n = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).filter(|&p| bright(p)).count();
    Ok(n as f64 / (w * h).max(1) as f64)
  }

  pub fn act(&mut self, act: Act) -> io::Result<()> {
    let window = self.view.window().ok_or_else(|| bad("the screen isn't shown"))?;
    let now = NSProcessInfo::processInfo().systemUptime();
    let number = window.windowNumber();

    let (kind, x, y, clicks) = match act {
      Act::Key(code, down) => return self.key(code, down, number, now),
      Act::Move(x, y) => (NSEventType::MouseMoved, x, y, 0),
      Act::Button { x, y, right, down, clicks } => {
        let kind = match (right, down) {
          (false, true) => NSEventType::LeftMouseDown,
          (false, false) => NSEventType::LeftMouseUp,
          (true, true) => NSEventType::RightMouseDown,
          (true, false) => NSEventType::RightMouseUp,
        };

        (kind, x, y, clicks)
      }
    };

    let at = self.point(x, y)?;
    let pressure = if matches!(kind, NSEventType::LeftMouseDown | NSEventType::RightMouseDown) {
      1.0
    } else {
      0.0
    };

    let e = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
      kind,
      at,
      NSEventModifierFlags::empty(),
      now,
      number,
      None,
      0,
      clicks,
      pressure,
    );
    let mut e = e.ok_or_else(|| bad("no mouse event"))?;

    // An event made this way says button 0 whatever its type, and VZ reads the button from it.
    if matches!(kind, NSEventType::RightMouseDown | NSEventType::RightMouseUp) {
      let cg = e.CGEvent().ok_or_else(|| bad("no mouse event"))?;
      CGEvent::set_integer_value_field(Some(&cg), CGEventField::MouseEventButtonNumber, 1);
      e = NSEvent::eventWithCGEvent(&cg).ok_or_else(|| bad("no mouse event"))?;
    }

    match kind {
      NSEventType::LeftMouseDown => self.view.mouseDown(&e),
      NSEventType::LeftMouseUp => self.view.mouseUp(&e),
      NSEventType::RightMouseDown => self.view.rightMouseDown(&e),
      NSEventType::RightMouseUp => self.view.rightMouseUp(&e),
      _ => self.view.mouseMoved(&e),
    }

    Ok(())
  }

  /// A key down or up; a modifier as `flagsChanged`, as a keyboard sends it, with its device bit:
  /// VZ reads a modifier flag without one as the key released.
  fn key(&mut self, code: u16, down: bool, window: isize, now: f64) -> io::Result<()> {
    let modifier = MODIFIERS.iter().find(|x| x.0 == code);
    if let Some((_, flag, bit)) = modifier {
      let both = NSEventModifierFlags(flag.0 | bit);
      match down {
        true => self.flags |= both,
        false => self.flags &= NSEventModifierFlags(!both.0),
      }
    }

    let kind = match (modifier.is_some(), down) {
      (true, _) => NSEventType::FlagsChanged,
      (false, true) => NSEventType::KeyDown,
      (false, false) => NSEventType::KeyUp,
    };

    let empty = NSString::new();
    let e = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
      kind,
      NSPoint::new(0.0, 0.0),
      self.flags,
      now,
      window,
      None,
      &empty,
      &empty,
      false,
      code,
    );
    let e = e.ok_or_else(|| bad("no key event"))?;

    match kind {
      NSEventType::FlagsChanged => self.view.flagsChanged(&e),
      NSEventType::KeyDown => self.view.keyDown(&e),
      _ => self.view.keyUp(&e),
    }

    Ok(())
  }

  /// The shot's size now: the display's, which the guest may have changed since it started.
  fn size(&self) -> (f64, f64) {
    let Screen { width, height, scale } = self.screen;
    let Some(surface) = surface(&self.view) else { return (width, height) };
    // SAFETY: as in `image`.
    let surface = unsafe { &*(Retained::as_ptr(&surface) as *const IOSurfaceRef) };
    (surface.width() as f64 / scale, surface.height() as f64 / scale)
  }

  /// A shot's pixel in the view's window: the view shows the display scaled to fit, centered.
  fn point(&self, x: f64, y: f64) -> io::Result<NSPoint> {
    let (w, h) = self.size();
    if x >= w || y >= h {
      return Err(bad(format!("({x}, {y}) is outside the {w}×{h} screen")));
    }

    let b = self.view.bounds().size;
    let k = (b.width / w).min(b.height / h);
    let (ox, oy) = ((b.width - w * k) / 2.0, (b.height - h * k) / 2.0);
    let (px, py) = (ox + (x + 0.5) * k, oy + (y + 0.5) * k);
    let py = if self.view.isFlipped() { py } else { b.height - py };
    Ok(self.view.convertPoint_toView(NSPoint::new(px, py), None))
  }

  /// Shows the VM in a normal window, made the first time; the app gets a Dock icon meanwhile.
  pub fn open(&mut self, mtm: MainThreadMarker) -> io::Result<()> {
    let app = NSApplication::sharedApplication(mtm);
    let window = match &self.window {
      Some(x) => x.clone(),
      None => {
        let Screen { width: w, height: h, .. } = self.screen;
        let area =
          NSScreen::mainScreen(mtm).map(|x| x.visibleFrame().size).unwrap_or(NSSize::new(w, h));
        let k = (area.width * 0.9 / w).min(area.height * 0.9 / h).min(1.0);
        let style = NSWindowStyleMask::Titled
          | NSWindowStyleMask::Closable
          | NSWindowStyleMask::Miniaturizable
          | NSWindowStyleMask::Resizable;
        // SAFETY: a window made and used on the main thread, kept in `self.window`.
        let window = unsafe {
          NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w * k, h * k)),
            style,
            NSBackingStoreType::Buffered,
            false,
          )
        };

        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str(&self.title));
        window.setContentAspectRatio(NSSize::new(w, h));
        self.view.removeFromSuperview();
        window.setContentView(Some(&self.view));
        window.center();
        self.window = Some(window.clone());
        window
      }
    };

    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    window.makeKeyAndOrderFront(None);
    window.makeFirstResponder(Some(&self.view));
    app.activate();
    Ok(())
  }

  /// Puts the view back in the tiny window once the person has closed the VM's window.
  pub fn tick(&mut self, mtm: MainThreadMarker) {
    let closed = self.window.as_ref().is_some_and(|x| !x.isVisible() && !x.isMiniaturized());
    if !closed {
      return;
    }

    if let Some(window) = self.window.take() {
      window.setContentView(None);
    }

    let Screen { width: w, height: h, .. } = self.screen;
    self.view.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h)));
    self.holder.addSubview(&self.view);
    NSApplication::sharedApplication(mtm)
      .setActivationPolicy(NSApplicationActivationPolicy::Accessory);
  }
}

/// The IOSurface the view's layers show: VZ's framebuffer.
fn surface(view: &NSView) -> Option<Retained<AnyObject>> {
  let cls = AnyClass::get(c"IOSurface")?;

  fn find(layer: &CALayer, cls: &AnyClass) -> Option<Retained<AnyObject>> {
    // SAFETY: reading a layer's contents and sublayers on the main thread.
    if let Some(x) = unsafe { layer.contents() } {
      // SAFETY: isKindOfClass: on an object.
      let ok: bool = unsafe { msg_send![&*x, isKindOfClass: cls] };
      if ok {
        return Some(x);
      }
    }

    let subs = unsafe { layer.sublayers() }?;
    subs.iter().find_map(|x| find(&x, cls))
  }

  if let Some(x) = view.layer().and_then(|x| find(&x, cls)) {
    return Some(x);
  }

  view.subviews().iter().find_map(|x| surface(&x))
}

// MARK: Keys

/// Modifier key codes, their flag and their left-hand device bit (IOKit's NX_DEVICE*KEYMASK).
const MODIFIERS: [(u16, NSEventModifierFlags, usize); 4] = [
  (0x37, NSEventModifierFlags::Command, 0x08),
  (0x38, NSEventModifierFlags::Shift, 0x02),
  (0x3A, NSEventModifierFlags::Option, 0x20),
  (0x3B, NSEventModifierFlags::Control, 0x01),
];

/// macOS's virtual key codes of the US layout's keys, as the characters they type unshifted.
const CHARS: &[(char, u16)] = &[
  ('a', 0),
  ('s', 1),
  ('d', 2),
  ('f', 3),
  ('h', 4),
  ('g', 5),
  ('z', 6),
  ('x', 7),
  ('c', 8),
  ('v', 9),
  ('b', 11),
  ('q', 12),
  ('w', 13),
  ('e', 14),
  ('r', 15),
  ('y', 16),
  ('t', 17),
  ('1', 18),
  ('2', 19),
  ('3', 20),
  ('4', 21),
  ('6', 22),
  ('5', 23),
  ('=', 24),
  ('9', 25),
  ('7', 26),
  ('-', 27),
  ('8', 28),
  ('0', 29),
  (']', 30),
  ('o', 31),
  ('u', 32),
  ('[', 33),
  ('i', 34),
  ('p', 35),
  ('l', 37),
  ('j', 38),
  ('\'', 39),
  ('k', 40),
  (';', 41),
  ('\\', 42),
  (',', 43),
  ('/', 44),
  ('n', 45),
  ('m', 46),
  ('.', 47),
  (' ', 49),
  ('`', 50),
];

/// The characters typed with Shift, and the key's unshifted character.
const SHIFTED: &[(char, char)] = &[
  ('~', '`'),
  ('!', '1'),
  ('@', '2'),
  ('#', '3'),
  ('$', '4'),
  ('%', '5'),
  ('^', '6'),
  ('&', '7'),
  ('*', '8'),
  ('(', '9'),
  (')', '0'),
  ('_', '-'),
  ('+', '='),
  ('{', '['),
  ('}', ']'),
  ('|', '\\'),
  (':', ';'),
  ('"', '\''),
  ('<', ','),
  ('>', '.'),
  ('?', '/'),
];

const NAMES: &[(&str, u16)] = &[
  ("enter", 36),
  ("return", 36),
  ("tab", 48),
  ("space", 49),
  ("backspace", 51),
  ("esc", 53),
  ("escape", 53),
  ("cmd", 0x37),
  ("command", 0x37),
  ("super", 0x37),
  ("win", 0x37),
  ("meta", 0x37),
  ("shift", 0x38),
  ("capslock", 57),
  ("alt", 0x3A),
  ("option", 0x3A),
  ("opt", 0x3A),
  ("ctrl", 0x3B),
  ("control", 0x3B),
  ("f1", 122),
  ("f2", 120),
  ("f3", 99),
  ("f4", 118),
  ("f5", 96),
  ("f6", 97),
  ("f7", 98),
  ("f8", 100),
  ("f9", 101),
  ("f10", 109),
  ("f11", 103),
  ("f12", 111),
  ("home", 115),
  ("pageup", 116),
  ("delete", 117),
  ("del", 117),
  ("end", 119),
  ("pagedown", 121),
  ("left", 123),
  ("right", 124),
  ("down", 125),
  ("up", 126),
];

const SHIFT: u16 = 0x38;

/// The keys that type `c`: Shift first when it needs it.
pub fn chord(c: char) -> Option<Vec<u16>> {
  let code = |c: char| CHARS.iter().find(|x| x.0 == c).map(|x| x.1);
  match c {
    '\n' => Some(vec![36]),
    '\t' => Some(vec![48]),
    'A'..='Z' => Some(vec![SHIFT, code(c.to_ascii_lowercase())?]),
    _ => match SHIFTED.iter().find(|x| x.0 == c) {
      Some((_, base)) => Some(vec![SHIFT, code(*base)?]),
      None => Some(vec![code(c)?]),
    },
  }
}

/// `cmd+shift+t`'s key codes, in order; a character typed with Shift brings it.
pub fn combo(spec: &str) -> io::Result<Vec<u16>> {
  let key = |name: &str| -> Option<Vec<u16>> {
    let lower = name.to_ascii_lowercase();
    if let Some((_, code)) = NAMES.iter().find(|x| x.0 == lower) {
      return Some(vec![*code]);
    }

    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
      (Some(c), None) => chord(c.to_ascii_lowercase()),
      _ => None,
    }
  };

  // `+` alone, or last after another `+`, is the key itself.
  let spec = spec.trim();
  let mut names: Vec<&str> = spec.split('+').collect();
  if spec.ends_with("++") || spec == "+" {
    names.retain(|x| !x.is_empty());
    names.push("+");
  }

  let mut codes = Vec::new();
  for x in names {
    for code in key(x).ok_or_else(|| bad(format!("unknown key {x:?} in {spec:?}")))? {
      if !codes.contains(&code) {
        codes.push(code);
      }
    }
  }

  Ok(codes)
}

pub type Do<'a> = &'a dyn Fn(Act) -> io::Result<()>;

fn pause(ms: u64) {
  thread::sleep(Duration::from_millis(ms));
}

/// Presses the keys in order and releases them in reverse.
fn press(codes: &[u16], act: Do) -> io::Result<()> {
  for code in codes {
    act(Act::Key(*code, true))?;
    pause(5);
  }

  for code in codes.iter().rev() {
    act(Act::Key(*code, false))?;
    pause(5);
  }

  Ok(())
}

pub fn pointer(x: f64, y: f64, right: Option<bool>, clicks: isize, act: Do) -> io::Result<String> {
  act(Act::Move(x, y))?;
  pause(50);
  let Some(right) = right else { return Ok(String::new()) };
  for n in 1..=clicks.clamp(1, 3) {
    act(Act::Button { x, y, right, down: true, clicks: n })?;
    pause(30);
    act(Act::Button { x, y, right, down: false, clicks: n })?;
    pause(60);
  }

  Ok(String::new())
}

pub fn typing(text: &str, act: Do) -> io::Result<String> {
  let chords: Vec<Vec<u16>> = text
    .chars()
    .map(|c| {
      chord(c).ok_or_else(|| bad(format!("can't type {c:?}: only the US layout's characters")))
    })
    .collect::<io::Result<_>>()?;

  for x in chords {
    press(&x, act)?;
    pause(10);
  }

  Ok(String::new())
}

pub fn keys(spec: &str, act: Do) -> io::Result<String> {
  press(&combo(spec)?, act)?;
  Ok(String::new())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn chords() {
    assert_eq!(chord('a'), Some(vec![0]));
    assert_eq!(chord('A'), Some(vec![SHIFT, 0]));
    assert_eq!(chord('!'), Some(vec![SHIFT, 18]));
    assert_eq!(chord('\n'), Some(vec![36]));
    assert_eq!(chord('é'), None);
    for c in (' '..='~').filter(|x| *x != '\x7f') {
      assert!(chord(c).is_some(), "{c:?}");
    }
  }

  #[test]
  fn combos() {
    assert_eq!(combo("cmd+space").unwrap(), vec![0x37, 49]);
    assert_eq!(combo("Ctrl+Alt+T").unwrap(), vec![0x3B, 0x3A, 17]);
    assert_eq!(combo("enter").unwrap(), vec![36]);
    assert_eq!(combo("cmd++").unwrap(), vec![0x37, SHIFT, 24]);
    assert_eq!(combo("cmd+?").unwrap(), vec![0x37, SHIFT, 44]);
    assert_eq!(combo("shift+!").unwrap(), vec![SHIFT, 18]);
    assert!(combo("cmd+nope").is_err());
  }
}
