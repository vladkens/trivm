// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! The Setup Assistant of macOS 13 and 14, answered from the host as a person would: each screen
//! known by a line of its text in a shot read with Vision, then its buttons clicked by their text;
//! then Remote Login turned on in Terminal, so `start` goes on over SSH as for the others. Their
//! Data volume is encrypted once installed, so the files that skip the assistant on 15 and 26
//! can't be written from the host.

use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};
use std::{env, fs, io, thread};

use crate::vision::{self, Line};
use crate::vm::{PASSWORD, Vm};
use crate::{bad, vz};

/// The file in a VM's folder that leaves its Setup Assistant to trivm, until SSH answers.
pub const MARK: &str = "assistant";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
  /// The button or label of that text, the topmost of them.
  Click(&'static str),
  /// The button of that text, else Return, the default button's key.
  Go(&'static str),
  Type(&'static str),
  /// The full name `User`, which makes the account `user`, in the field right of its label, then
  /// the password twice.
  Account,
}

use Step::*;

/// The screens by a line of their text, dialogs first: one shows over its screen, whose title is
/// still read beside it. As on 13.6 and 14.6.1 in a VM.
const SCREENS: &[(&str, &[Step])] = &[
  ("Are you sure you want to skip", &[Click("Skip")]),
  ("I have read and agree to the", &[Click("Agree")]),
  ("Are you sure you don't want to", &[Click("Don't Use")]),
  ("Language", &[Click("English"), Go("→")]),
  (
    "Select Your Country or Region",
    &[Type("United States"), Click("United States"), Go("Continue")],
  ),
  ("Written and Spoken Languages", &[Go("Continue")]),
  ("Accessibility", &[Click("Not Now")]),
  ("Data & Privacy", &[Go("Continue")]),
  ("Migration Assistant", &[Click("Not Now")]),
  ("Sign In with Your Apple ID", &[Click("Set Up Later")]),
  ("Terms and Conditions", &[Click("Agree")]),
  ("Create a Computer Account", &[Account, Go("Continue")]),
  ("Enable Location Services", &[Go("Continue")]),
  ("Select Your Time Zone", &[Go("Continue")]),
  ("Analytics", &[Click("Share Mac Analytics with Apple"), Go("Continue")]),
  ("Screen Time", &[Click("Set Up Later")]),
  ("Choose Your Look", &[Go("Continue")]),
];

/// The buttons that go on from a screen not in the list, in this order.
const ONWARD: &[&str] = &["Continue", "Not Now", "Set Up Later"];

/// What a shot shows.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Seen {
  /// An app's menu bar, its last menu Help: the assistant is done.
  Desktop,
  /// A screen at work, as while the account is made.
  Busy,
  /// A screen in the list, by its index.
  Screen(usize),
  /// The first screen: hello written by hand, and Get Started in each language in turn under
  /// its round button.
  Hello,
  /// A screen not in the list, and whether it has a button that goes on.
  Other(bool),
}

/// The lines of that text, in any case, top to bottom.
fn find<'a>(lines: &'a [Line], text: &str) -> impl Iterator<Item = &'a Line> {
  lines.iter().filter(move |x| x.text.trim().eq_ignore_ascii_case(text))
}

/// The hello screen's label under its button, in the middle at the bottom of the screen.
fn hello(lines: &[Line]) -> Option<&Line> {
  lines.iter().find(|x| x.y > 790.0 && (x.x - 800.0).abs() < 120.0)
}

fn seen(lines: &[Line]) -> Seen {
  if lines.iter().any(|x| x.y < 30.0 && x.text.ends_with("Help")) {
    return Seen::Desktop;
  }

  if lines.iter().any(|x| x.text.contains("Creating account")) {
    return Seen::Busy;
  }

  if let Some(i) = SCREENS.iter().position(|(title, _)| find(lines, title).next().is_some()) {
    return Seen::Screen(i);
  }

  match hello(lines) {
    Some(_) => Seen::Hello,
    None => Seen::Other(ONWARD.iter().any(|x| find(lines, x).next().is_some())),
  }
}

/// Answers the assistant from whatever screen it shows, the screens in any order; then Remote
/// Login on. A screen is answered once two shots a second apart read the same, and again, up to
/// three times, if it still shows 30 s after.
pub fn answer(vm: &Vm) -> io::Result<()> {
  eprintln!("answering {}'s Setup Assistant from its screens (about 5 minutes)", vm.name);
  let shot = env::temp_dir().join(format!("trivm-{}-setup.png", vm.name));
  let fail = |msg: String| bad(format!("Setup Assistant: {msg}; its screen: {}", shot.display()));
  let t = Instant::now();
  let (mut before, mut acted) = (None, None::<(Seen, Instant, u32)>);
  loop {
    if vm.pid().is_none() {
      return Err(fail(format!("{} stopped", vm.name)));
    }

    if t.elapsed() > Duration::from_secs(1800) {
      return Err(fail("not done in 30 minutes".into()));
    }

    thread::sleep(Duration::from_secs(1));
    // No frame: the guest hasn't drawn yet, or the assistant left alone has put the display to
    // sleep, which a key wakes (the pointer doesn't).
    if vz::ask(vm, "shot", &shot.to_string_lossy()).is_err() {
      let _ = vz::ask(vm, "key shift", "");
      continue;
    }

    let lines = vision::lines(&shot)?;
    let now = seen(&lines);
    if now == Seen::Desktop {
      break;
    }

    let settled = before == Some(now);
    before = Some(now);
    if !settled || matches!(now, Seen::Busy | Seen::Other(false)) {
      continue;
    }

    let tries = match acted {
      Some((x, at, _)) if x == now && at.elapsed() < Duration::from_secs(30) => continue,
      Some((x, _, n)) if x == now => n + 1,
      _ => 1,
    };

    if tries > 3 {
      return Err(fail(format!("it stays on {}", name(now))));
    }

    eprintln!("  {}", name(now));
    act(vm, now, &lines, &shot)?;
    acted = Some((now, Instant::now(), tries));
  }

  remote_login(vm, &shot)?;
  let _ = fs::remove_file(&shot);
  Ok(())
}

fn name(x: Seen) -> &'static str {
  match x {
    Seen::Screen(i) => SCREENS[i].0,
    Seen::Hello => "hello",
    _ => "a screen trivm doesn't know",
  }
}

/// Does a screen's steps. A step after the first finds its text in a new shot, as the steps
/// before may have changed the screen, or else where the first shot had it.
fn act(vm: &Vm, seen: Seen, lines: &[Line], shot: &Path) -> io::Result<()> {
  let click = |(x, y): (f64, f64)| vz::ask(vm, &format!("click {x} {y} left 1"), "").map(|_| ());
  let steps: &[Step] = match seen {
    Seen::Screen(i) => SCREENS[i].1,
    // The round button, 39 points above the label.
    Seen::Hello => return hello(lines).map_or(Ok(()), |x| click((x.x, x.y - 39.0))),
    Seen::Other(_) => {
      let next = ONWARD.iter().find_map(|x| find(lines, x).next());
      return next.map_or(Ok(()), |x| click((x.x, x.y)));
    }
    _ => &[],
  };

  let at = |first: bool, text: &str| -> io::Result<Option<(f64, f64)>> {
    for _ in 0..if first { 0 } else { 3 } {
      vz::ask(vm, "shot", &shot.to_string_lossy())?;
      if let Some(x) = find(&vision::lines(shot)?, text).next() {
        return Ok(Some((x.x, x.y)));
      }

      thread::sleep(Duration::from_millis(500));
    }

    Ok(find(lines, text).next().map(|x| (x.x, x.y)))
  };

  for (i, step) in steps.iter().enumerate() {
    match *step {
      Click(text) => match at(i == 0, text)? {
        Some(x) => click(x)?,
        // Tried again once the screen has had its time.
        None => return Ok(()),
      },
      Go(text) => match at(i == 0, text)? {
        Some(x) => click(x)?,
        None => vz::ask(vm, "key enter", "").map(|_| ())?,
      },
      Type(text) => vz::ask(vm, "type", text).map(|_| ())?,
      Account => {
        let Some((x, y)) = at(i == 0, "Full name:")? else { return Ok(()) };
        click((x + 150.0, y))?;
        vz::ask(vm, "key cmd+a", "")?;
        vz::ask(vm, "type", &format!("User\t\t{PASSWORD}\t{PASSWORD}"))?;
      }
    }
  }

  Ok(())
}

/// Remote Login on from Terminal, opened by Spotlight, sudo given the user's password; Terminal
/// quit once SSH answers.
fn remote_login(vm: &Vm, shot: &Path) -> io::Result<()> {
  let up = || {
    let addr = vm.ip().and_then(|x| format!("{x}:22").parse::<SocketAddr>().ok());
    addr.is_some_and(|x| TcpStream::connect_timeout(&x, Duration::from_secs(2)).is_ok())
  };
  if up() {
    return Ok(());
  }

  eprintln!("  Remote Login, in Terminal");
  let key = |x: &str| vz::ask(vm, &format!("key {x}"), "").map(|_| ());
  let typing = |x: &str| vz::ask(vm, "type", x).map(|_| ());
  let screen = |text: &str| vision::wait_text(vm, shot, text, Duration::from_secs(60));
  let wait = |what: &str, done: &dyn Fn(&[Line]) -> bool| -> io::Result<()> {
    let t = Instant::now();
    loop {
      if vz::ask(vm, "shot", &shot.to_string_lossy()).is_ok() && done(&vision::lines(shot)?) {
        return Ok(());
      }

      if t.elapsed() > Duration::from_secs(60) {
        return Err(bad(format!("no {what} in a minute; its screen: {}", shot.display())));
      }

      thread::sleep(Duration::from_secs(1));
    }
  };

  key("cmd+space")?;
  wait("Spotlight", &|x| x.iter().any(|x| x.text.contains("Spotlight")))?;
  typing("Terminal")?;
  wait("Terminal in Spotlight", &|x| hit(x).is_some())?;

  // Return opens the top hit; 13's Spotlight opens it only clicked.
  key("enter")?;
  if vision::wait_text(vm, shot, "Last login", Duration::from_secs(10)).is_err() {
    vz::ask(vm, "shot", &shot.to_string_lossy())?;
    if let Some(x) = hit(&vision::lines(shot)?) {
      vz::ask(vm, &format!("click {} {} left 1", x.x, x.y), "")?;
    }

    screen("Last login")?;
  }

  typing("sudo launchctl load -w /System/Library/LaunchDaemons/ssh.plist\n")?;
  screen("Password:")?;
  typing(&format!("{PASSWORD}\n"))?;

  let t = Instant::now();
  while !up() {
    if t.elapsed() > Duration::from_secs(60) {
      return Err(bad(format!("SSH doesn't answer after Remote Login; {}", shot.display())));
    }

    thread::sleep(Duration::from_secs(1));
  }

  key("cmd+q")
}

/// Spotlight's hit for Terminal: the line of it under the query's.
fn hit(lines: &[Line]) -> Option<&Line> {
  let mut found = lines.iter().filter(|x| x.text.contains("Terminal"));
  let query = found.next()?;
  found.find(|x| x.y - query.y > 10.0)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn lines(x: &[(&str, f64, f64)]) -> Vec<Line> {
    x.iter().map(|(text, x, y)| Line { text: text.to_string(), x: *x, y: *y }).collect()
  }

  #[test]
  fn screens() {
    // As read on 14.6.1.
    let hello = lines(&[("bo", 545.0, 449.0), ("Démarrer", 802.0, 822.0)]);
    assert_eq!(seen(&hello), Seen::Hello);
    let country = lines(&[
      ("Select Your Country or Region", 799.0, 348.0),
      ("Back", 1062.0, 721.0),
      ("Continue", 1148.0, 722.0),
      ("Press Command-Option-F5 to view accessibility options.", 800.0, 821.0),
    ]);
    assert_eq!(seen(&country), Seen::Screen(4));
    let skip = lines(&[
      ("Sign In with Your Apple ID", 800.0, 347.0),
      ("Are you sure you want to skip", 801.0, 440.0),
      ("Don't Skip", 742.0, 544.0),
      ("Skip", 859.0, 544.0),
    ]);
    assert_eq!(seen(&skip), Seen::Screen(0));
    assert_eq!(find(&skip, "skip").map(|x| x.x).collect::<Vec<_>>(), [859.0]);

    let agree = lines(&[("Agree", 859.0, 526.0), ("Agree", 1148.0, 722.0)]);
    assert_eq!(find(&agree, "Agree").next().map(|x| x.y), Some(526.0));
    let busy = lines(&[
      ("Create a Computer Account", 800.0, 238.0),
      ("→ Creating account...", 490.0, 722.0),
    ]);
    assert_eq!(seen(&busy), Seen::Busy);
    let desktop = lines(&[("Finder File Edit View Go Window Help", 215.0, 12.0)]);
    assert_eq!(seen(&desktop), Seen::Desktop);
    let terminal = lines(&[("Terminal Shell Edit View Window Help", 206.0, 12.0)]);
    assert_eq!(seen(&terminal), Seen::Desktop);
    assert_eq!(seen(&lines(&[("Language Chooser", 115.0, 13.0)])), Seen::Other(false));
    assert_eq!(
      seen(&lines(&[("Siri", 800.0, 348.0), ("Continue", 1148.0, 722.0)])),
      Seen::Other(true)
    );
    assert_eq!(seen(&lines(&[])), Seen::Other(false));

    // As read on 13.6: the query, completed, then the hit.
    let spotlight = lines(&[
      ("Terminal - Open", 585.0, 206.0),
      ("Terminal", 552.0, 207.0),
      ("Terminal", 529.0, 252.0),
      ("Siri Suggested Websites", 552.0, 293.0),
    ]);
    assert_eq!(hit(&spotlight).map(|x| x.y), Some(252.0));
    assert_eq!(hit(&spotlight[..2]), None);
  }
}
