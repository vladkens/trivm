// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! The guest's desktop as named elements: agent-desktop, which trivm puts in the guest, reads its
//! windows' accessibility trees (AX on macOS, AT-SPI on Linux, UI Automation on Windows); `ui`
//! prints them a line an element, with its role, name and frame in a shot's pixels, and
//! `click --name` clicks one.

use std::collections::HashSet;
use std::io::Write;
use std::process::Stdio;
use std::{fmt, fs, io};

use crate::system::Desktop;
use crate::vm::{Os, Vm};
use crate::{bad, permissions, run, ssh, system, windows};

/// agent-desktop (vercel-labs/agent-desktop, Apache-2.0): its release's arm64 archives.
const VERSION: &str = "1.3.1";
const MACOS: (&str, &str) = (
  "https://github.com/vercel-labs/agent-desktop/releases/download/v1.3.1/agent-desktop-v1.3.1-aarch64-apple-darwin.tar.gz",
  "9b7b7cfc74892eab46c2c85d6cb7dc4863bf8dab0a222cfcfb8ae4604c69c86c",
);
const LINUX: (&str, &str) = (
  "https://github.com/vercel-labs/agent-desktop/releases/download/v1.3.1/agent-desktop-v1.3.1-aarch64-unknown-linux-gnu.tar.gz",
  "438990a703fbeb19775596c341c3701435e80adbfa1b2f64b82ff17567d5573b",
);
const WINDOWS: (&str, &str) = (
  "https://github.com/vercel-labs/agent-desktop/releases/download/v1.3.1/agent-desktop-v1.3.1-aarch64-pc-windows-msvc.tar.gz",
  "805f1f61890f9983fceaf38cda94a37a049c379435d65607db5cedd22c8ef97b",
);

/// The Visual C++ runtime that agent-desktop's Windows build links to, which Windows doesn't
/// have: Microsoft's installer for arm64, 14.52.37029, its cache name, URL and SHA-256.
const VC_REDIST: (&str, &str, &str) = (
  "vc_redist-14.52.37029.arm64.exe",
  "https://download.visualstudio.microsoft.com/download/pr/78d061c5-9ef3-42ab-98ed-3309d1095dc4/3C81BCF5B06156858362400D7066F65ECE38E8ACCB5E687FC65BEF5735F31FFC/VC_redist.arm64.exe",
  "3c81bcf5b06156858362400d7066f65ece38e8accb5e687fc65bef5735f31ffc",
);

/// Where agent-desktop is in the guest.
const BIN: &str = "/usr/local/bin/agent-desktop";
const WINDOWS_BIN: &str = r"C:\Program Files\agent-desktop\agent-desktop.exe";

/// How deep a window's tree is read, in agent-desktop's levels, which skip nameless wrappers.
const DEPTH: u32 = 30;

/// Exits 3 unless agent-desktop `$1` is in the guest.
const HAS: &str = r#"case "$(/usr/local/bin/agent-desktop version 2>/dev/null)" in
  *"\"version\":\"$1\""*) ;;
  *) exit 3 ;;
esac
"#;

/// Exits 4 unless TCC's database allows agent-desktop Accessibility, which root reads with SIP on
/// too. Run without it, it would have macOS put up its prompt to allow it on the guest's screen
/// at its first call to AX.
const ALLOWED: &str = r#"tcc='/Library/Application Support/com.apple.TCC/TCC.db'
q="SELECT auth_value FROM access WHERE service = 'kTCCServiceAccessibility' AND client = '/usr/local/bin/agent-desktop'"
[ "$(sudo -n sqlite3 "$tcc" "$q" 2>/dev/null)" = 2 ] || exit 4
"#;

/// Runs agent-desktop with `$2`, its arguments as a plist's `<string>`s, as a launchd job in the
/// user's GUI session: TCC holds a process started over SSH to sshd-keygen-wrapper's grants, a
/// job to its own. Prints its output once it exits, or after a minute, when it is stopped.
const MACOS_RUN: &str = r#"set -e
job=trivm.ui.$$
d=$(mktemp -d)
cat > "$d/job.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>Label</key><string>$job</string>
<key>ProgramArguments</key><array><string>/usr/local/bin/agent-desktop</string>$2</array>
<key>StandardOutPath</key><string>$d/out</string>
<key>RunAtLoad</key><true/>
</dict></plist>
EOF
launchctl bootstrap gui/$(id -u) "$d/job.plist"
n=0
until launchctl print gui/$(id -u)/$job | grep -Eq 'last exit code = -?[0-9]'; do
  n=$((n + 1))
  [ $n -lt 1200 ] || break
  sleep 0.05
done
launchctl bootout gui/$(id -u)/$job
cat "$d/out"
rm -rf "$d"
"#;

/// Runs agent-desktop, `$exe`, by the batch file's line on stdin in the desktop's session as
/// `desktop` runs a program, in a console with no window to take the focus; prints its output once
/// it exits, failing after a minute. Exits 3 unless it is `$version`.
const WINDOWS_RUN: &str = r#"if ("$(& $exe version 2>$null)" -notlike "*`"version`":`"$version`"*") { exit 3 }
$dir = (New-Item -ItemType Directory "$env:TEMP\trivm-ui-$([guid]::NewGuid())").FullName
# cmd reads a batch file in the console's code page: UTF-8 after `chcp`.
$line = [Console]::In.ReadToEnd().TrimEnd()
[IO.File]::WriteAllText("$dir\run.cmd", "@chcp 65001 >nul`r`n@$line >out 2>err`r`n@echo.>done`r`n")
$run = New-ScheduledTaskAction -Execute "$env:windir\System32\conhost.exe" `
  -Argument '--headless cmd.exe /d /v:off /c run.cmd' -WorkingDirectory $dir
$user = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive -RunLevel Highest
$set = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit 0
$name = "trivm-ui-$([guid]::NewGuid())"
Register-ScheduledTask -TaskName $name -Action $run -Principal $user -Settings $set | Out-Null
try {
  Start-ScheduledTask -TaskName $name
  for ($i = 0; $i -lt 600 -and -not (Test-Path "$dir\done"); $i++) { Start-Sleep -Milliseconds 100 }
  if (-not (Test-Path "$dir\done")) {
    Stop-ScheduledTask -TaskName $name
    throw 'agent-desktop ran for a minute; stopped'
  }
  [Console]::Error.Write([IO.File]::ReadAllText("$dir\err"))
  [Console]::Out.Write([IO.File]::ReadAllText("$dir\out"))
} finally {
  Unregister-ScheduledTask -TaskName $name -Confirm:$false
  Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
}
"#;

/// Prints X's active window, then the windows shown on the screen, top to bottom, popups too:
/// `window <id> <x> <y> <width> <height>`, a client's in a window manager's frame, then the frame
/// around it, GTK's shadow inside it, the window it's for and its title, as `xprop` prints them.
const STACK: &str = r#"set -e
for x in xprop xwininfo; do
  command -v $x >/dev/null || { echo "click --name needs $x in the guest" >&2; exit 1; }
done
xprop -root -notype _NET_ACTIVE_WINDOW
frames=$(for id in $(xprop -root _NET_CLIENT_LIST_STACKING | sed 's/.*# //; s/,/ /g'); do
  echo "$(xwininfo -id "$id" -tree | awk '/Parent window id/ { print $4 }') $id"
done)
for top in $(xwininfo -root -children | awk '$1 ~ /^0x/ { print $1 }'); do
  id=$(echo "$frames" | awk -v top="$top" '$1 == top { print $2 }')
  id=${id:-$top}
  g=$(xwininfo -id "$id" | awk '/Absolute upper-left X/ { x = $NF } /Absolute upper-left Y/ { y = $NF }
    /Width:/ { w = $NF } /Height:/ { h = $NF } /IsViewable/ { print x, y, w, h }')
  [ -n "$g" ] || continue
  echo "window $id $g"
  xprop -id "$id" -notype _NET_FRAME_EXTENTS _GTK_FRAME_EXTENTS WM_TRANSIENT_FOR _NET_WM_NAME || true
done
"#;

/// Puts agent-desktop, its archive on stdin, in the guest.
const INSTALL: &str = "set -e
sudo -n mkdir -p /usr/local/bin
sudo -n tar -xzf - --no-same-owner -C /usr/local/bin agent-desktop
";

/// Puts agent-desktop, its archive on stdin, in a Windows guest: Windows' own `tar`, run by cmd,
/// sshd's shell, which leaves stdin alone.
const WINDOWS_INSTALL: &str = r#"mkdir "C:\Program Files\agent-desktop" 2>nul & tar -xzf - -C "C:\Program Files\agent-desktop" agent-desktop.exe"#;

/// Where `scp` puts the Visual C++ runtime's installer, in the user's home, and its run, quiet.
const VC_COPY: &str = "AppData/Local/Temp/trivm-vc_redist.exe";
const VC_INSTALL: &str = r#"$f = "$env:TEMP\trivm-vc_redist.exe"
$p = Start-Process $f '/install /quiet /norestart' -Wait -PassThru
# The installer, or Defender scanning it, may hold it for a while after it exits: left in TEMP then.
foreach ($i in 1..20) { try { Remove-Item $f -ErrorAction Stop; break } catch { Start-Sleep -Milliseconds 500 } }
# 1638: a newer one is there; 3010: done, a restart asked for files in use.
if ($p.ExitCode -notin 0, 1638, 3010) { throw "the Visual C++ runtime's installer failed: $($p.ExitCode)" }
"#;

/// Switches accessibility on in the Linux session: AT-SPI's `IsEnabled`, which Chromium, Qt and
/// AccessKit wait for before they show their elements (GTK shows them regardless).
const A11Y: &str = "gsettings set org.gnome.desktop.interface toolkit-accessibility true\n";

/// `ui <vm> [--app <name>]`: the visible windows, all apps' or one's, and their elements.
pub fn ui(name: &str, app: Option<&str>) -> io::Result<()> {
  let vm = running(name, "ui")?;
  let nodes = tree(&vm, &windows(&vm)?, app)?;
  if nodes.is_empty() {
    eprintln!("no windows on {name}'s desktop");
  }

  for x in nodes {
    println!("{x}");
  }

  Ok(())
}

/// `click <vm> --name <text>`: the element of that name clicked at its frame's middle, as
/// `click <x> <y>` does, its window raised first as a person brings it to the front; on Linux an
/// error when another window X shows covers that point: a dialog, an always-on-top one, a popup.
pub fn click(
  name: &str,
  text: &str,
  app: Option<&str>,
  right: bool,
  double: bool,
) -> io::Result<()> {
  let vm = running(name, "click --name")?;
  let (mut nodes, mut at, mut all) = element(&vm, text, app, None)?;

  let window = window_of(&nodes, &nodes[at]);
  if !in_front(&nodes, window) {
    // agent-desktop stops waiting for the focus at 750 ms, which macOS in a VM can pass.
    let id = window.id.clone();
    let raised = match vm.config.os() {
      Os::Windows => raise(&vm, &id),
      _ => agent_desktop(&vm, &["focus-window", "--window-id", &id]).map(|_| ()),
    };

    if let Err(e) = raised
      && !windows(&vm)?.iter().any(|w| w.id == id && w.focused())
    {
      return Err(e);
    }

    // Read again, that window only: it may lay its elements out anew once it has the focus.
    (nodes, at, all) = element(&vm, text, app, Some(&id))?;
  }

  let (node, window) = (&nodes[at], window_of(&nodes, &nodes[at]));
  // Past the right or the bottom, the display refuses the click: it knows its size now.
  let (x, y) = node.middle();
  if x < 0.0 || y < 0.0 {
    return Err(bad(format!("{} is off the screen", node.line())));
  }

  // agent-desktop lists the focused window first, but X keeps a dialog or an always-on-top window
  // above it and doesn't list popups. X's window at the point must be the window, told from a
  // twin of its title and frame by X's focus, or the popup of the menu the element is in.
  if vm.config.os() == Os::Linux && !window.id.is_empty() {
    let (active, all) = stack(&ssh::output(&vm, &format!("sh -c {}", ssh::quote(STACK)))?);
    let like = |c: &&Client| c.window.name == window.name && c.window.frame == window.frame;
    let mut twins: Vec<&Client> = all.iter().filter(like).collect();
    if twins.is_empty() {
      twins = all.iter().filter(|c| c.window.name == window.name).collect();
    }

    let own = |id: &str| twins.iter().any(|c| c.id == id) && (twins.len() == 1 || id == active);
    match all.iter().find(|c| c.window.covers(x, y)) {
      Some(c) if own(&c.id) => {}
      Some(c) if own(&c.owner) && in_menu(&nodes, node, c.window.frame) => {}
      Some(c) if twins.len() > 1 && twins.iter().any(|t| t.id == c.id) => {
        let line = window.line();
        return Err(bad(format!(
          "{line} has a twin of its title and frame, and X's focus is on neither"
        )));
      }
      Some(c) => return Err(bad(format!("{} is under {}", node.line(), c.window.line()))),
      None => return Err(bad(format!("{} is on none of X's windows", node.line()))),
    }
  }

  // Windows keeps an always-on-top window in front of the one raised, and a popup in front of the
  // window it's for: the first window at the point, from the front, must be the element's.
  if vm.config.os() == Os::Windows && !window.id.is_empty() {
    match all.iter().find(|w| w.covers(x, y)) {
      Some(w) if w.id == window.id => {}
      Some(w) => return Err(bad(format!("{} is under {}", node.line(), w.line()))),
      None => return Err(bad(format!("{} is out of its window", node.line()))),
    }
  }

  let (button, clicks) = (if right { "right" } else { "left" }, if double { 2 } else { 1 });
  crate::ask(&vm, &format!("click {x} {y} {button} {clicks}"), "")?;
  eprintln!("clicked {} at {x},{y}", node.line());
  Ok(())
}

/// Brings a Windows window to the front, minimized and restored as its taskbar button does: Windows
/// refuses `focus-window`'s `SetForegroundWindow` to a program in the background, as agent-desktop
/// is in its task, but lets a window it restores take the focus. `focus-window` then checks it.
fn raise(vm: &Vm, id: &str) -> io::Result<()> {
  let args = format!(r#"{{"window_id":{}}}"#, json_str(id));
  let batch: Vec<String> = ["minimize", "restore", "focus-window"]
    .iter()
    .map(|x| format!(r#"{{"command":"{x}","args":{args}}}"#))
    .collect();
  let done = agent_desktop(vm, &["batch", "--stop-on-error", &format!("[{}]", batch.join(","))])?;
  let mut errors = done.get("results").items().iter().map(|x| x.get("error").get("message").str());
  match errors.find(|x| !x.is_empty()) {
    Some(msg) => Err(bad(format!("agent-desktop in {}: {msg}", vm.name))),
    None => Ok(()),
  }
}

/// The windows' elements, all apps' or one's, and where the one named `text` is among them, or
/// among the elements of the window with the id given: an error when none or more than one is.
/// Then all the windows, front to back.
fn element(
  vm: &Vm,
  text: &str,
  app: Option<&str>,
  window: Option<&str>,
) -> io::Result<(Vec<Node>, usize, Vec<Node>)> {
  let all = windows(vm)?;
  let nodes = tree(vm, &all, app)?;
  let (mut name, mut some) = (vm.name.clone(), &nodes[..]);
  if let Some(id) = window {
    some = in_window(&nodes, id).ok_or_else(|| bad(format!("{text:?}: its window left {name}")))?;
    name = some[0].line();
  }

  let at = match find(some, text)[..] {
    [one] => nodes.iter().position(|x| std::ptr::eq(x, one)).unwrap_or(0),
    [] => return Err(bad(format!("no element named {text:?} in {name}"))),
    ref found => {
      let lines: Vec<String> = found.iter().map(|x| format!("\n  {}", x.line())).collect();
      let (n, lines) = (found.len(), lines.concat());
      return Err(bad(format!(
        "{n} elements named {text:?} in {name}; click one by its frame:{lines}"
      )));
    }
  };

  Ok((nodes, at, all))
}

/// The window with the id and its elements, as `tree` lists them.
fn in_window<'a>(nodes: &'a [Node], id: &str) -> Option<&'a [Node]> {
  let start = nodes.iter().position(|x| x.depth == 0 && x.id == id)?;
  let rest = nodes[start + 1..].iter().position(|x| x.depth == 0);
  Some(&nodes[start..rest.map_or(nodes.len(), |n| start + 1 + n)])
}

/// The running VM for `cmd`, refused on a system agent-desktop doesn't work in.
fn running(name: &str, cmd: &str) -> io::Result<Vm> {
  let vm = Vm::open(name)?;
  if !supported(&vm)? {
    return Err(unsupported(&vm, cmd));
  }

  match vm.pid() {
    Some(_) => Ok(vm),
    None => Err(bad(format!("{name} isn't running; trivm start {name}"))),
  }
}

/// Whether trivm puts agent-desktop in the VM's system: not in Ubuntu 22.04's Plasma, as
/// agent-desktop needs 24.04's AT-SPI, and found no X window to raise for Konsole's.
fn supported(vm: &Vm) -> io::Result<bool> {
  Ok(match vm.config.os() {
    Os::Linux => system::find(&vm.config.system)?.desktop != Desktop::Plasma,
    _ => true,
  })
}

/// `cmd` refused on a system agent-desktop isn't put in by trivm.
fn unsupported(vm: &Vm, cmd: &str) -> io::Error {
  bad(format!("{cmd}: not supported on {}", vm.config.system))
}

/// The visible windows, front to back, as agent-desktop lists them.
fn windows(vm: &Vm) -> io::Result<Vec<Node>> {
  let all = agent_desktop(vm, &["list-windows"])?;
  let shown =
    |w: &&Json| w.get("visible").bool() && w.get("accessible").bool() && !w.get("minimized").bool();
  let window = |w: &Json| Node {
    id: w.get("id").str().into(),
    app: w.get("app_name").str().into(),
    role: "window".into(),
    name: w.get("title").str().into(),
    states: w.get("is_focused").bool().then(|| "focused".to_string()).into_iter().collect(),
    frame: frame(w.get("bounds")).unwrap_or_default(),
    ..Node::default()
  };

  Ok(all.items().iter().filter(shown).map(window).collect())
}

/// A window X shows on a Linux guest's screen: a client, a popup, one with no accessibility.
struct Client {
  id: String,
  /// The window it's for: a dialog's or popup's.
  owner: String,
  window: Node,
}

/// X's active window and the windows it shows, top to bottom, from `STACK`'s lines. A frame is
/// as agent-desktop gives a window's: the window manager's frame in, GTK's shadow out.
fn stack(out: &str) -> (String, Vec<Client>) {
  let id = |x: &str| -> String {
    let id = x.strip_prefix("window id # ").and_then(|x| x.split(',').next());
    id.unwrap_or_default().into()
  };
  let sides = |x: &str| -> [f64; 4] {
    let n: Option<Vec<f64>> =
      x.split([' ', ',']).filter(|x| !x.is_empty()).map(|x| x.parse().ok()).collect();
    n.and_then(|n| n.try_into().ok()).unwrap_or_default()
  };

  let (mut active, mut all) = (String::new(), Vec::<Client>::new());
  for line in out.lines() {
    if let Some((x, rest)) = line.strip_prefix("window ").and_then(|x| x.split_once(' ')) {
      let window = Node { role: "window".into(), frame: sides(rest), ..Node::default() };
      all.push(Client { id: x.into(), owner: String::new(), window });
      continue;
    }

    let Some((key, value)) = line.split_once([':', '=']) else { continue };
    let (key, value) = (key.trim(), value.trim());
    if key == "_NET_ACTIVE_WINDOW" {
      active = id(value);
    }

    let Some(c) = all.last_mut() else { continue };
    let ([l, r, t, b], [x, y, w, h]) = (sides(value), c.window.frame);
    match key {
      "_NET_FRAME_EXTENTS" => c.window.frame = [x - l, y - t, w + l + r, h + t + b],
      "_GTK_FRAME_EXTENTS" => c.window.frame = [x + l, y + t, w - l - r, h - t - b],
      "WM_TRANSIENT_FOR" => c.owner = id(value),
      "_NET_WM_NAME" => {
        let name = value.strip_prefix('"').and_then(|x| x.strip_suffix('"')).unwrap_or_default();
        c.window.name = unescape(name);
      }
      _ => {}
    }
  }

  (active, all)
}

/// A string as `xprop` prints it, quoted: `\\`, `\"`, `\n`, `\t` and other control characters as
/// `\` and three octal digits.
fn unescape(x: &str) -> String {
  let (mut out, mut chars) = (String::new(), x.chars());
  while let Some(c) = chars.next() {
    let next = if c == '\\' { chars.next() } else { None };
    match next {
      None => out.push(c),
      Some('n') => out.push('\n'),
      Some('t') => out.push('\t'),
      Some(d @ '0'..='7') => {
        let digits: String = [Some(d), chars.next(), chars.next()].into_iter().flatten().collect();
        out.push(u8::from_str_radix(&digits, 8).map_or('\u{fffd}', char::from));
      }
      Some(x) => out.push(x),
    }
  }

  out
}

/// The windows, all apps' or one's, each followed by its elements, after the menu bar on macOS,
/// the frontmost app's: agent-desktop reads their trees in one batch.
fn tree(vm: &Vm, all: &[Node], app: Option<&str>) -> io::Result<Vec<Node>> {
  let exe = vm.config.os() == Os::Windows;
  let wanted = |x: &str| app.is_none_or(|a| same_app(x, a, exe));
  let windows: Vec<&Node> = all.iter().filter(|w| wanted(&w.app)).collect();

  let snapshot = |scope: String| {
    format!(
      r#"{{"command":"snapshot","args":{{{scope},"include_bounds":true,"compact":true,"max_depth":{DEPTH}}}}}"#
    )
  };
  let window = |w: &&Node| snapshot(format!(r#""window_id":{}"#, json_str(&w.id)));
  let mut batch: Vec<String> = windows.iter().map(window).collect();
  let menubar = vm.config.os() == Os::Macos;
  if menubar {
    batch.push(snapshot(r#""surface":"menubar""#.into()));
  }

  let mut out = Vec::new();
  if !batch.is_empty() {
    let done = agent_desktop(vm, &["batch", &format!("[{}]", batch.join(","))])?;
    let results = done.get("results").items();
    let bar = results.get(windows.len()).filter(|_| menubar).map_or(&NULL, |x| x.get("data"));
    let front = bar.get("app").str();
    if !front.is_empty() && wanted(front) {
      let frame = frame(bar.get("tree").get("bounds")).unwrap_or_default();
      let head = Node { app: front.into(), role: "menubar".into(), frame, ..Node::default() };
      add(&mut out, head, bar);
    }

    for (w, res) in windows.iter().zip(results) {
      match res.get("error").get("message").str() {
        "" => add(&mut out, (*w).clone(), res.get("data")),
        msg => eprintln!("trivm: {}: {msg}", w.line()),
      }
    }
  }

  once(&mut out);
  if let Some(app) = app
    && out.is_empty()
  {
    let mut apps: Vec<&str> = all.iter().map(|w| w.app.as_str()).collect();
    apps.sort();
    apps.dedup();
    return Err(bad(format!(
      "no window of {app} in {}; apps with windows: {}",
      vm.name,
      apps.join(", ")
    )));
  }

  Ok(out)
}

/// Whether a window's app is the one named: in any case, and with `exe`, as on Windows, which
/// names an app by its program, `Notepad.exe`, with `.exe` left out or not.
fn same_app(app: &str, name: &str, exe: bool) -> bool {
  fn bare(x: &str, exe: bool) -> &str {
    let cut = x.len().saturating_sub(4);
    match x.get(cut..) {
      Some(ext) if exe && ext.eq_ignore_ascii_case(".exe") => &x[..cut],
      _ => x,
    }
  }

  bare(app, exe).eq_ignore_ascii_case(bare(name, exe))
}

/// Leaves out a popup's element alike in all but its window to one in a window before, unless
/// something in it stays: Windows lists a popup's in the popup's own window, in front, and in the
/// window it's for, under other elements there.
fn once(nodes: &mut Vec<Node>) {
  let (mut seen, mut keep) = (HashSet::new(), vec![true; nodes.len()]);
  let starts: Vec<usize> = (0..nodes.len()).filter(|&i| nodes[i].depth == 0).collect();
  for (n, &start) in starts.iter().enumerate() {
    let end = starts.get(n + 1).copied().unwrap_or(nodes.len());
    for i in (start + 1..end).rev() {
      let inside = nodes[i + 1..end].iter().take_while(|x| x.depth > nodes[i].depth).count();
      let twice = nodes[i].popup && seen.contains(&nodes[i].line());
      keep[i] = !twice || keep[i + 1..=i + inside].contains(&true);
    }

    let popups = nodes[start + 1..end].iter().filter(|x| x.popup);
    seen.extend(popups.map(|x| x.line()));
  }

  let mut keep = keep.into_iter();
  nodes.retain(|_| keep.next().unwrap_or(true));
}

/// A window's or menu bar's line, then the elements of its snapshot.
fn add(out: &mut Vec<Node>, head: Node, snapshot: &Json) {
  if !snapshot.get("complete").bool() {
    eprintln!("trivm: {}: its tree was cut short", head.line());
  }

  out.push(head);
  for x in snapshot.get("tree").get("children").items() {
    flatten(x, 1, out);
  }
}

/// agent-desktop's answer to the arguments in the running guest, its `data`. It is installed
/// first when it isn't there, and on macOS granted Accessibility when it lacks it.
fn agent_desktop(vm: &Vm, args: &[&str]) -> io::Result<Json> {
  let (line, input) = command(vm, args)?;
  let (mut installed, mut granted) = (false, false);
  let out = loop {
    let mut ssh = ssh::ssh(vm)?;
    ssh.arg("--").arg(&line).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = ssh.spawn()?;
    // Dropped once written: the end of the input.
    if let Some(mut x) = child.stdin.take() {
      x.write_all(input.as_bytes())?;
    }

    let out = child.wait_with_output()?;
    match out.status.code() {
      Some(3) if !installed => {
        install(vm)?;
        installed = true;
      }
      Some(4) if !granted => {
        grant(vm)?;
        granted = true;
      }
      _ => break out,
    }
  };

  let text = String::from_utf8_lossy(&out.stdout);
  let Some(reply) = Json::parse(&text) else {
    let err = String::from_utf8_lossy(&out.stderr);
    let err = if text.trim().is_empty() { err.trim() } else { text.trim() };
    return Err(bad(format!("agent-desktop in {} ({}): {err}", vm.name, out.status)));
  };

  match reply.get("ok").bool() {
    true => Ok(reply.get("data").clone()),
    false => {
      let msg = reply.get("error").get("message").str();
      Err(bad(format!("agent-desktop in {}: {msg}", vm.name)))
    }
  }
}

/// The guest's command line that runs agent-desktop with the arguments, and its input.
fn command(vm: &Vm, args: &[&str]) -> io::Result<(String, String)> {
  let sh =
    |script: String, rest: String| format!("sh -c {} ui {VERSION} {rest}", ssh::quote(&script));
  Ok(match vm.config.os() {
    Os::Macos => {
      (sh(format!("{HAS}{ALLOWED}{MACOS_RUN}"), ssh::quote(&plist_strings(args))), String::new())
    }
    Os::Linux => {
      let args: Vec<_> = args.iter().map(|x| ssh::quote(x)).collect();
      (sh(format!("{HAS}shift\nexec {BIN} \"$@\"\n"), args.join(" ")), String::new())
    }
    Os::Windows => {
      let line = windows::batch_line(&[&[WINDOWS_BIN], args].concat());
      if format!("@{line} >out 2>err").len() > windows::CMD_MAX {
        return Err(bad(format!(
          "{} windows' trees are past cmd's line: --app picks fewer",
          vm.name
        )));
      }

      let exe = windows::quote(WINDOWS_BIN);
      (windows::powershell(&format!("$version = '{VERSION}'\n$exe = {exe}\n{WINDOWS_RUN}"))?, line)
    }
  })
}

/// Puts agent-desktop in the guest from trivm's cache; on Linux switches accessibility on.
fn install(vm: &Vm) -> io::Result<()> {
  let ((url, sha256), script) = match vm.config.os() {
    Os::Macos => (MACOS, INSTALL.to_string()),
    Os::Linux => (LINUX, format!("{INSTALL}{A11Y}")),
    Os::Windows => (WINDOWS, WINDOWS_INSTALL.to_string()),
  };

  let archive = system::download(url, sha256)?;
  eprintln!("installing agent-desktop {VERSION} in {}", vm.name);
  if vm.config.os() == Os::Windows {
    vc_runtime(vm)?;
  }

  run(ssh::ssh(vm)?.arg("--").arg(script).stdin(fs::File::open(&archive)?))?;
  Ok(())
}

/// Installs the Visual C++ runtime in a Windows guest unless it has it, from Microsoft's installer.
fn vc_runtime(vm: &Vm) -> io::Result<()> {
  if ssh::output(vm, r"Test-Path $env:windir\System32\vcruntime140.dll")? == "True" {
    return Ok(());
  }

  let (name, url, sha256) = VC_REDIST;
  let exe = system::download_as(url, name, sha256)?;
  eprintln!("installing the Visual C++ runtime in {}", vm.name);
  if !ssh::cp(&exe.to_string_lossy(), &format!("{}:{VC_COPY}", vm.name))?.success() {
    return Err(bad(format!("copying {name} into {} failed", vm.name)));
  }

  ssh::output(vm, VC_INSTALL).map(|_| ())
}

/// Accessibility for agent-desktop by its path, as `os grant` gives it, with SIP off. The
/// grant has no code requirement, so another version keeps it.
fn grant(vm: &Vm) -> io::Result<()> {
  let services = permissions::services(&["accessibility".into()])?;
  let res = permissions::allow(vm, BIN, &services);
  res.map(|_| ()).map_err(|e| bad(format!("agent-desktop needs Accessibility in {}: {e}", vm.name)))
}

/// Arguments as a plist's `<string>`s.
fn plist_strings(args: &[&str]) -> String {
  let escape = |x: &str| x.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
  args.iter().map(|x| format!("<string>{}</string>", escape(x))).collect()
}

// MARK: Tree

/// An element as `ui` prints it: a window at depth 0, its elements under it.
#[derive(Clone, Debug, Default, PartialEq)]
struct Node {
  depth: usize,
  /// agent-desktop's id of a window; none for the menu bar and an element.
  id: String,
  /// The app, a window's.
  app: String,
  role: String,
  name: String,
  value: String,
  states: Vec<String>,
  /// x, y, width and height in a shot's pixels.
  frame: [f64; 4],
  /// Whether it is in a popup, a menu's on Windows, which may be listed twice.
  popup: bool,
}

impl Node {
  fn focused(&self) -> bool {
    self.states.iter().any(|x| x == "focused")
  }

  /// The middle of its frame, where a click goes.
  fn middle(&self) -> (f64, f64) {
    let [x, y, w, h] = self.frame;
    ((x + w / 2.0).round(), (y + h / 2.0).round())
  }

  /// Whether the point is in its frame.
  fn covers(&self, x: f64, y: f64) -> bool {
    let [left, top, w, h] = self.frame;
    (left..left + w).contains(&x) && (top..top + h).contains(&y)
  }

  /// Its line without the indent.
  fn line(&self) -> String {
    self.to_string().trim_start().to_string()
  }
}

impl fmt::Display for Node {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    write!(f, "{}", "  ".repeat(self.depth))?;
    if !self.app.is_empty() {
      write!(f, "{}: ", self.app)?;
    }

    write!(f, "{}", self.role)?;
    if !self.name.is_empty() {
      write!(f, " {:?}", cut(&self.name))?;
    }

    if !self.value.is_empty() {
      write!(f, " = {:?}", cut(&self.value))?;
    }

    if !self.states.is_empty() {
      write!(f, " [{}]", self.states.join(" "))?;
    }

    let [x, y, w, h] = self.frame.map(|x| x.round() as i64);
    write!(f, " {x},{y} {w}x{h}")
  }
}

/// Text cut to 100 characters for a line.
fn cut(x: &str) -> String {
  match x.char_indices().nth(100) {
    Some((i, _)) => format!("{}…", &x[..i]),
    None => x.to_string(),
  }
}

/// The elements of a window's tree worth a line, depth first: one with no frame is left out but
/// not its children, a hidden one with its children, a closed menu (with no frame) with its
/// items, and a nameless leaf with no value.
fn flatten(node: &Json, depth: usize, out: &mut Vec<Node>) {
  let states: Vec<String> = node.get("states").items().iter().map(|x| x.str().into()).collect();
  let (role, name) = (node.get("role").str(), node.get("name").str());
  // Windows' popup, a menu's, is a window with no frame, off the screen, its items on it.
  let popup = role == "window" && frame(node.get("bounds")).is_none();
  if states.iter().any(|x| x == "hidden" || (x == "offscreen" && !popup)) {
    return;
  }

  let children = node.get("children").items();
  let Some(frame) = frame(node.get("bounds")) else {
    // A closed menu's custom views (Finder's tags) have frames at the screen's corner.
    if role == "menu" {
      return;
    }

    let at = out.len();
    for x in children {
      flatten(x, depth, out);
    }

    if popup {
      out[at..].iter_mut().for_each(|x| x.popup = true);
    }

    return;
  };

  // A toggle's value is its state, which `checked` says.
  let value = match node.get("value").str() {
    x if x == name || ["checkbox", "switch", "radiobutton"].contains(&role) => "",
    x => x,
  };

  let at = out.len();
  let (role, name, value) = (role.into(), name.to_string(), value.to_string());
  let nameless = name.is_empty() && value.is_empty();
  out.push(Node { depth, role, name, value, states, frame, ..Node::default() });
  for x in children {
    flatten(x, depth + 1, out);
  }

  if nameless && out.len() == at + 1 {
    out.pop();
  }
}

/// x, y, width and height of agent-desktop's bounds; none for an empty frame.
fn frame(bounds: &Json) -> Option<[f64; 4]> {
  let [x, y, w, h] = ["x", "y", "width", "height"].map(|k| bounds.get(k).num());
  let frame = [x?, y?, w?, h?];
  (frame[2] > 0.0 && frame[3] > 0.0).then_some(frame)
}

/// The elements whose name is `text`, else is it ignoring case, else holds it ignoring case;
/// windows are not elements to click.
fn find<'a>(nodes: &'a [Node], text: &str) -> Vec<&'a Node> {
  let low = text.to_lowercase();
  let tiers: [&dyn Fn(&Node) -> bool; 3] =
    [&|x| x.name == text, &|x| x.name.to_lowercase() == low, &|x| {
      x.name.to_lowercase().contains(&low)
    }];
  tiers.iter().map(|f| outermost(nodes, f)).find(|x| !x.is_empty()).unwrap_or_default()
}

/// The elements that match, a match counted once with the matches inside it when they all have its
/// name: GTK 4 names a file's cell or row, a cell in it, its icon and its label alike, one element
/// to click. Else each is counted, it too.
fn outermost<'a>(nodes: &'a [Node], hit: &dyn Fn(&Node) -> bool) -> Vec<&'a Node> {
  let (mut out, mut i) = (Vec::new(), 0);
  while i < nodes.len() {
    let x = &nodes[i];
    if x.depth == 0 || !hit(x) {
      i += 1;
      continue;
    }

    out.push(x);
    let inside = &nodes[i + 1..];
    let last = i + 1 + inside.iter().position(|y| y.depth <= x.depth).unwrap_or(inside.len());
    let same = nodes[i + 1..last].iter().filter(|y| hit(y)).all(|y| y.name == x.name);
    i = if same { last } else { i + 1 };
  }

  out
}

/// Whether the element is an item of an open menu whose items all lie in `frame`: the popup the
/// menu shows them in.
fn in_menu(nodes: &[Node], node: &Node, frame: [f64; 4]) -> bool {
  let at = nodes.iter().position(|x| std::ptr::eq(x, node)).unwrap_or(0);
  let Some(menu) = nodes[..at].iter().rposition(|x| x.depth < node.depth) else { return false };
  let depth = nodes[menu].depth;
  let mut items = nodes[menu + 1..].iter().take_while(|x| x.depth > depth);
  let [left, top, w, h] = frame;
  let inside = |x: &Node| {
    let [l, t, iw, ih] = x.frame;
    l >= left && t >= top && l + iw <= left + w && t + ih <= top + h
  };

  nodes[menu].role == "menu" && items.all(|x| x.depth > depth + 1 || inside(x))
}

/// Whether the window is in front: the menu bar, with no id, one focused, or one listed before the
/// focused one, a popup on Windows, which agent-desktop lists from the front: raised, it'd close.
fn in_front(nodes: &[Node], window: &Node) -> bool {
  let at = nodes.iter().position(|x| std::ptr::eq(x, window)).unwrap_or(0);
  window.id.is_empty() || nodes[at..].iter().any(|x| x.depth == 0 && x.focused())
}

/// The window or menu bar an element is in: the last line at depth 0 up to it.
fn window_of<'a>(nodes: &'a [Node], node: &'a Node) -> &'a Node {
  let at = nodes.iter().position(|x| std::ptr::eq(x, node)).unwrap_or(0);
  nodes[..=at].iter().rev().find(|x| x.depth == 0).unwrap_or(node)
}

// MARK: JSON

/// A JSON value, as agent-desktop prints them.
#[derive(Clone, Debug, PartialEq)]
enum Json {
  Null,
  Bool(bool),
  Num(f64),
  Str(String),
  Arr(Vec<Json>),
  Obj(Vec<(String, Json)>),
}

static NULL: Json = Json::Null;

impl Json {
  fn parse(text: &str) -> Option<Self> {
    let mut p = Parser { s: text.as_bytes(), i: 0 };
    let x = p.value()?;
    p.space();
    (p.i == p.s.len()).then_some(x)
  }

  /// An object's field; null when there's none.
  fn get(&self, key: &str) -> &Json {
    match self {
      Json::Obj(x) => x.iter().find(|(k, _)| k == key).map_or(&NULL, |(_, v)| v),
      _ => &NULL,
    }
  }

  fn items(&self) -> &[Json] {
    match self {
      Json::Arr(x) => x,
      _ => &[],
    }
  }

  fn str(&self) -> &str {
    match self {
      Json::Str(x) => x,
      _ => "",
    }
  }

  fn num(&self) -> Option<f64> {
    match self {
      Json::Num(x) => Some(*x),
      _ => None,
    }
  }

  fn bool(&self) -> bool {
    matches!(self, Json::Bool(true))
  }
}

struct Parser<'a> {
  s: &'a [u8],
  i: usize,
}

impl Parser<'_> {
  fn space(&mut self) {
    while self.s.get(self.i).is_some_and(|x| x.is_ascii_whitespace()) {
      self.i += 1;
    }
  }

  fn eat(&mut self, x: &str) -> Option<()> {
    let found = self.s[self.i..].starts_with(x.as_bytes());
    found.then(|| self.i += x.len())
  }

  fn value(&mut self) -> Option<Json> {
    self.space();
    match self.s.get(self.i)? {
      b'n' => self.eat("null").map(|_| Json::Null),
      b't' => self.eat("true").map(|_| Json::Bool(true)),
      b'f' => self.eat("false").map(|_| Json::Bool(false)),
      b'"' => self.string().map(Json::Str),
      b'[' => self.array(),
      b'{' => self.object(),
      _ => self.number(),
    }
  }

  fn array(&mut self) -> Option<Json> {
    self.eat("[")?;
    let mut items = Vec::new();
    self.space();
    if self.eat("]").is_some() {
      return Some(Json::Arr(items));
    }

    loop {
      items.push(self.value()?);
      self.space();
      if self.eat(",").is_none() {
        self.eat("]")?;
        return Some(Json::Arr(items));
      }
    }
  }

  fn object(&mut self) -> Option<Json> {
    self.eat("{")?;
    let mut fields = Vec::new();
    self.space();
    if self.eat("}").is_some() {
      return Some(Json::Obj(fields));
    }

    loop {
      self.space();
      let key = self.string()?;
      self.space();
      self.eat(":")?;
      fields.push((key, self.value()?));

      self.space();
      if self.eat(",").is_none() {
        self.eat("}")?;
        return Some(Json::Obj(fields));
      }
    }
  }

  fn number(&mut self) -> Option<Json> {
    let start = self.i;
    let num = |x: &u8| x.is_ascii_digit() || b"-+.eE".contains(x);
    while self.s.get(self.i).is_some_and(num) {
      self.i += 1;
    }

    std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok().map(Json::Num)
  }

  fn string(&mut self) -> Option<String> {
    self.eat("\"")?;
    let mut out = Vec::new();
    loop {
      let c = *self.s.get(self.i)?;
      self.i += 1;
      match c {
        b'"' => return String::from_utf8(out).ok(),
        b'\\' => out.extend_from_slice(self.escape()?.encode_utf8(&mut [0; 4]).as_bytes()),
        _ => out.push(c),
      }
    }
  }

  /// The character an escape in a string stands for, after its backslash.
  fn escape(&mut self) -> Option<char> {
    let e = *self.s.get(self.i)?;
    self.i += 1;
    let c = match e {
      b'"' | b'\\' | b'/' => e as char,
      b'b' => '\u{8}',
      b'f' => '\u{c}',
      b'n' => '\n',
      b'r' => '\r',
      b't' => '\t',
      b'u' => {
        let u = self.hex()?;
        if !(0xd800..0xdc00).contains(&u) {
          return char::from_u32(u);
        }

        // A character past the BMP is a pair of surrogates, each escaped.
        self.eat("\\u")?;
        let low = self.hex()?.checked_sub(0xdc00).filter(|x| *x < 0x400)?;
        char::from_u32(0x10000 + ((u - 0xd800) << 10) + low)?
      }
      _ => return None,
    };

    Some(c)
  }

  fn hex(&mut self) -> Option<u32> {
    let x = std::str::from_utf8(self.s.get(self.i..self.i + 4)?).ok()?;
    self.i += 4;
    u32::from_str_radix(x, 16).ok()
  }
}

/// `x` as a JSON string.
fn json_str(x: &str) -> String {
  let mut out = String::from("\"");
  for c in x.chars() {
    match c {
      '"' | '\\' => {
        out.push('\\');
        out.push(c);
      }
      c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
      c => out.push(c),
    }
  }

  out.push('"');
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn json() {
    let x = Json::parse(r#" {"a": [1, -2.5e1, true, false, null], "b": {"c": "d"}, "e": []} "#);
    let x = x.unwrap();
    let a = x.get("a").items();
    assert_eq!(a.len(), 5);
    assert_eq!((a[0].num(), a[1].num()), (Some(1.0), Some(-25.0)));
    assert!(a[2].bool() && !a[3].bool());
    assert_eq!(a[4], Json::Null);
    assert_eq!(x.get("b").get("c").str(), "d");
    assert_eq!(x.get("e").items(), &[]);
    assert_eq!(x.get("none").get("deeper"), &Json::Null);

    let s = Json::parse(r#""a\"b\\c\/\n\t\u00e9\ud83d\ude00 ⌥…""#).unwrap();
    assert_eq!(s.str(), "a\"b\\c/\n\té😀 ⌥…");
    for bad in ["", "{", "[1,]", "{\"a\" 1}", "\"\\x\"", "\"\\ud800\\u0041\"", "1 2", "nul"] {
      assert_eq!(Json::parse(bad), None, "{bad}");
    }

    let tricky = "a\"b\\c\nd\u{1}é";
    assert_eq!(Json::parse(&json_str(tricky)).unwrap().str(), tricky);
  }

  /// A snapshot of Wavo's window on macOS, cut down: a radio button, a combo box, a title, a
  /// hidden element, a nameless group, a nameless leaf, a group with no frame and a closed menu.
  const WAVO: &str = r#"{"children":[
    {"bounds":{"height":16.0,"width":34.0,"x":783.0,"y":108.0},"name":"Wavo","role":"statictext","value":"Wavo"},
    {"bounds":{"height":48.0,"width":560.0,"x":520.0,"y":132.0},"role":"tablist","children":[
      {"bounds":{"height":48.0,"width":56.0,"x":653.5,"y":132.0},"name":"General","role":"radiobutton","states":["checked","selected"],"value":"1"},
      {"bounds":{"height":48.0,"width":56.0,"x":710.0,"y":132.0},"name":"Models","role":"radiobutton","value":"0"}]},
    {"bounds":{"height":24.0,"width":111.0,"x":939.0,"y":233.0},"name":"Microphone","role":"combobox","value":"Automatic"},
    {"name":"Hidden","role":"button","states":["offscreen"],"bounds":{"height":1.0,"width":1.0,"x":0.0,"y":0.0}},
    {"bounds":{"height":14.0,"width":14.0,"x":575.0,"y":109.0},"role":"group","children":[
      {"bounds":{"height":14.0,"width":14.0,"x":575.0,"y":109.0},"role":"group"}]},
    {"role":"tab","states":["selected"],"children":[
      {"bounds":{"height":458.0,"width":802.0,"x":5.0,"y":81.0},"name":"Terminal","role":"textfield"}]},
    {"bounds":{"height":0.0,"width":10.0,"x":5.0,"y":81.0},"name":"Flat","role":"button"},
    {"bounds":{"height":0.0,"width":0.0,"x":0.0,"y":900.0},"name":"File","role":"menu","children":[
      {"bounds":{"height":22.0,"width":24.0,"x":0.0,"y":878.0},"name":"Red","role":"radiobutton"}]}
  ]}"#;

  fn nodes() -> Vec<Node> {
    let mut out = Vec::new();
    for x in Json::parse(WAVO).unwrap().get("children").items() {
      flatten(x, 1, &mut out);
    }

    out
  }

  #[test]
  fn lines() {
    let lines: Vec<String> = nodes().iter().map(|x| x.to_string()).collect();
    assert_eq!(
      lines,
      [
        r#"  statictext "Wavo" 783,108 34x16"#,
        "  tablist 520,132 560x48",
        r#"    radiobutton "General" [checked selected] 654,132 56x48"#,
        r#"    radiobutton "Models" 710,132 56x48"#,
        r#"  combobox "Microphone" = "Automatic" 939,233 111x24"#,
        r#"  textfield "Terminal" 5,81 802x458"#,
      ]
    );

    let window = Node {
      depth: 0,
      id: "w-1".into(),
      app: "Wavo".into(),
      role: "window".into(),
      name: "a \"b\"\n".into(),
      value: String::new(),
      states: vec!["focused".into()],
      frame: [520.0, 100.0, 560.0, 593.0],
      popup: false,
    };
    assert_eq!(window.to_string(), r#"Wavo: window "a \"b\"\n" [focused] 520,100 560x593"#);
    assert!(window.covers(520.0, 100.0) && window.covers(1079.0, 692.0));
    assert!(!window.covers(1080.0, 300.0) && !window.covers(600.0, 99.0));
    assert_eq!(cut(&"é".repeat(101)), format!("{}…", "é".repeat(100)));
    assert_eq!(cut("short"), "short");
  }

  #[test]
  fn names() {
    let nodes = nodes();
    let names = |x: &str| -> Vec<String> { find(&nodes, x).iter().map(|x| x.line()).collect() };
    assert_eq!(names("Models"), [r#"radiobutton "Models" 710,132 56x48"#]);
    assert_eq!(find(&nodes, "Models")[0].middle(), (738.0, 156.0));
    assert_eq!(names("micro"), [r#"combobox "Microphone" = "Automatic" 939,233 111x24"#]);
    // One exact match, though "Wavo" is the window's title too.
    assert_eq!(names("Wavo"), [r#"statictext "Wavo" 783,108 34x16"#]);
    assert!(names("Hidden").is_empty());
    assert_eq!(names("e").len(), 4);

    // The whole name ignoring case before a part of another's.
    let button = |name: &str| Node { depth: 1, name: name.into(), ..Node::default() };
    let saves = [button("Save as"), button("Save"), button("SAVE")];
    let found = |x: &str| -> Vec<&str> { find(&saves, x).iter().map(|x| &x.name[..]).collect() };
    assert_eq!(
      (found("Save"), found("save"), found("sav")),
      (vec!["Save"], vec!["Save", "SAVE"], vec!["Save as", "Save", "SAVE"])
    );

    // GTK 4's cell, its twin and its label in Files are one, as a row, its icon and its label are;
    // the next is another. A group whose name holds that of the buttons in it is one more element.
    let el = |depth: usize, role: &str, name: &str| Node {
      depth,
      role: role.into(),
      name: name.into(),
      ..Node::default()
    };
    let cells = [
      el(0, "window", "Home"),
      el(1, "cell", "Videos"),
      el(2, "cell", "Videos"),
      el(3, "statictext", "Videos"),
      el(1, "cell", "Music"),
      el(1, "statictext", "Videos"),
      el(1, "row", "Templates"),
      el(2, "image", "Templates"),
      el(2, "statictext", "Templates"),
      el(1, "group", "Delete options"),
      el(2, "button", "Delete permanently"),
      el(3, "statictext", "Delete permanently"),
      el(2, "button", "Delete to Trash"),
      el(1, "group", "Remove all"),
      el(2, "button", "Remove one"),
    ];
    let roles = |x: &str| find(&cells, x).iter().map(|x| x.role.clone()).collect::<Vec<_>>();
    assert_eq!(roles("Videos"), ["cell", "statictext"]);
    assert_eq!(roles("Templates"), ["row"]);
    assert_eq!(roles("Delete"), ["group", "button", "button"]);
    assert_eq!(roles("Remove"), ["group", "button"]);

    // Two windows: the first's elements, then the second's.
    let mut nodes = nodes;
    let window =
      |id: &str| Node { depth: 0, id: id.into(), role: "window".into(), ..Node::default() };
    nodes.insert(4, window("w-2"));
    nodes.insert(0, window("w-1"));
    let of = |x: &str| window_of(&nodes, find(&nodes, x)[0]).id.clone();
    assert_eq!((of("General"), of("Microphone")), ("w-1".into(), "w-2".into()));

    // A window's own: "e" in the second's only, after a raise.
    let names = |id: &str| -> Vec<String> {
      find(in_window(&nodes, id).unwrap(), "e").iter().map(|x| x.line()).collect()
    };
    assert_eq!(names("w-1").len() + names("w-2").len(), 4);
    assert!(names("w-2").iter().all(|x| !x.contains("General")));
    assert!(in_window(&nodes, "w-3").is_none());
  }

  #[test]
  fn stacking() {
    let out = r#"_NET_ACTIVE_WINDOW: window id # 0x1200007, 0x0
window 0x120029a 634 345 296 270
_NET_FRAME_EXTENTS:  not found.
_GTK_FRAME_EXTENTS = 6, 6, 5, 7
WM_TRANSIENT_FOR: window id # 0x1200007
_NET_WM_NAME = "Thunar"
window 0x1200007 636 320 648 489
_NET_FRAME_EXTENTS = 1, 1, 24, 1
_GTK_FRAME_EXTENTS:  not found.
WM_TRANSIENT_FOR:  not found.
_NET_WM_NAME = "a \"b\" \\"
window 0x2a00004 -26 7 870 568
_NET_FRAME_EXTENTS = 0, 0, 0, 0
_GTK_FRAME_EXTENTS = 26, 26, 23, 29
_NET_WM_NAME = "Shadow\\n\n\t\001"
window 0x800036 -1 0 1 1080
_NET_WM_NAME:  not found."#;
    let (active, all) = stack(out);
    assert_eq!(active, "0x1200007");
    let lines: Vec<String> =
      all.iter().map(|c| format!("{} {} {}", c.id, c.owner, c.window.line())).collect();
    assert_eq!(
      lines,
      [
        r#"0x120029a 0x1200007 window "Thunar" 640,350 284x258"#,
        r#"0x1200007  window "a \"b\" \\" 635,296 650x514"#,
        r#"0x2a00004  window "Shadow\\n\n\t\u{1}" 0,30 818x516"#,
        "0x800036  window -1,0 1x1080"
      ]
    );
  }

  #[test]
  fn menus() {
    let item =
      |depth, role: &str, frame| Node { depth, role: role.into(), frame, ..Node::default() };
    let nodes = [
      item(0, "window", [635.0, 296.0, 650.0, 514.0]),
      item(1, "menu", [640.0, 325.0, 41.0, 25.0]),
      item(2, "menuitem", [640.0, 352.0, 284.0, 25.0]),
      item(3, "statictext", [0.0, 0.0, 1.0, 1.0]),
      item(2, "menuitem", [640.0, 581.0, 284.0, 25.0]),
      item(1, "button", [644.0, 354.0, 31.0, 31.0]),
    ];
    let popup = [640.0, 350.0, 284.0, 258.0];
    assert!(in_menu(&nodes, &nodes[2], popup) && in_menu(&nodes, &nodes[4], popup));
    assert!(!in_menu(&nodes, &nodes[4], [640.0, 350.0, 284.0, 200.0]));
    assert!(!in_menu(&nodes, &nodes[5], popup) && !in_menu(&nodes, &nodes[0], popup));
  }

  #[test]
  fn plist() {
    assert_eq!(
      plist_strings(&["batch", r#"[{"a":"<&>"}]"#]),
      r#"<string>batch</string><string>[{"a":"&lt;&amp;&gt;"}]</string>"#
    );
  }

  /// Notepad's Edit menu open on Windows, cut down: the popup in the tree of its own window, which
  /// has it under the menu bar's group, and in Notepad's, which has it under another.
  const POPUP: &str = r#"{"bounds":{"height":0.0,"width":0.0,"x":0.0,"y":0.0},"name":"Popup","role":"window","states":["offscreen","modal"],"children":[
    {"bounds":{"height":516.0,"width":232.0,"x":85.0,"y":100.0},"role":"menu","children":[
      {"bounds":{"height":29.0,"width":222.0,"x":90.0,"y":513.0},"name":"Select all","role":"menuitem"}]}]}"#;
  const BAR: &str =
    r#"{"bounds":{"height":33.0,"width":1428.0,"x":32.0,"y":68.0},"role":"group","children":[%]}"#;

  #[test]
  fn apps() {
    let same =
      [("Notepad.exe", "Notepad"), ("Notepad.exe", "notepad.EXE"), ("explorer.exe", "Explorer")];
    for (app, name) in same.into_iter().chain([("Wavo", "wavo"), ("ñexe", "ñexe")]) {
      assert!(same_app(app, name, true), "{app} {name}");
    }

    for (app, name) in [("Notepad.exe", "Note"), ("Wavo", "Wavo.app"), ("exe", "")] {
      assert!(!same_app(app, name, true), "{app} {name}");
    }

    // Elsewhere the name is the app's whole.
    assert!(same_app("Wavo", "WAVO", false) && !same_app("foo", "foo.exe", false));
  }

  #[test]
  fn popups() {
    let window = |id: &str, focused: bool| Node {
      id: id.into(),
      role: "window".into(),
      states: if focused { vec!["focused".into()] } else { vec![] },
      ..Node::default()
    };
    let tree = |json: &str, out: &mut Vec<Node>| flatten(&Json::parse(json).unwrap(), 1, out);

    let mut nodes = vec![window("w-1", false)];
    tree(&BAR.replace('%', POPUP), &mut nodes);
    nodes.push(window("w-2", true));
    let host = r#"{"bounds":{"height":536.0,"width":252.0,"x":75.0,"y":98.0},"name":"PopupHost","role":"group","children":[%]}"#;
    tree(&host.replace('%', POPUP), &mut nodes);
    let item = r#"{"bounds":{"height":32.0,"width":44.0,"x":85.0,"y":68.0},"name":"Edit","role":"menuitem"}"#;
    tree(&BAR.replace('%', item), &mut nodes);
    // A window of the same controls behind: they're no popup's, all listed.
    nodes.push(window("w-3", false));
    tree(&BAR.replace('%', item), &mut nodes);

    once(&mut nodes);
    let lines: Vec<String> = nodes.iter().map(|x| x.to_string()).collect();
    assert_eq!(
      lines,
      [
        "window 0,0 0x0",
        "  group 32,68 1428x33",
        "    menu 85,100 232x516",
        r#"      menuitem "Select all" 90,513 222x29"#,
        "window [focused] 0,0 0x0",
        r#"  group "PopupHost" 75,98 252x536"#,
        "  group 32,68 1428x33",
        r#"    menuitem "Edit" 85,68 44x32"#,
        "window 0,0 0x0",
        "  group 32,68 1428x33",
        r#"    menuitem "Edit" 85,68 44x32"#,
      ]
    );

    // The popup's window is in front, listed before the focused one; the one after isn't. Two
    // "Edit"s are, in two windows.
    let [popup, focused, behind] = [0, 4, 8].map(|i| in_front(&nodes, &nodes[i]));
    assert!(popup && focused && !behind);
    assert_eq!((find(&nodes, "Select all").len(), find(&nodes, "Edit").len()), (1, 2));
  }
}
