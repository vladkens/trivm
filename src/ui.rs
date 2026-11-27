// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! The guest's desktop as named elements: agent-desktop, which trivm puts in the guest, reads its
//! windows' accessibility trees (AX on macOS, AT-SPI on Linux); `ui` prints them a line an
//! element, with its role, name and frame in a shot's pixels, and `click --name` clicks one. On
//! Wayland the compositor says where the windows are: trivm's GNOME Shell extension, a KWin script.

use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{fmt, fs, io, mem};

use crate::system::Desktop;
use crate::vm::{Os, Vm};
use crate::{bad, permissions, run, ssh, system, vz};

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

/// Where agent-desktop is in the guest.
const BIN: &str = "/usr/local/bin/agent-desktop";

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

/// agent-desktop's session on Wayland, the desktop's, which the user's systemd has: over SSH it
/// would take X11's, and tell no window from Xwayland's.
const WAYLAND_ENV: &str =
  "export $(systemctl --user show-environment | grep -E '^(WAYLAND_DISPLAY|XDG_SESSION_TYPE)=')\n";

/// Prints the `inner` of each refmap of agent-desktop's snapshots `$@`, its own files, as a JSON
/// array: GTK 4 on Wayland gives an element's frame in its window's coordinates only, which
/// agent-desktop keeps there and leaves out of the snapshot.
const REFMAPS: &str = r#"printf '['
sep=
for id; do
  printf '%s' "$sep"
  cat ~/.agent-desktop/snapshots/"$id"/refmap.json 2>/dev/null || printf null
  sep=,
done
printf ']'
"#;

/// Runs the KWin script `$1` and prints what it logs after the mark `$2`, waiting up to 5 s for it:
/// KWin's scripts have no output on D-Bus, and `console.info` goes to the journal.
const KWIN: &str = r#"set -e
f=$(mktemp --suffix=.js)
printf '%s' "$1" > "$f"
t=$(date +%s.%N)
id=$(busctl --user call org.kde.KWin /Scripting org.kde.kwin.Scripting loadScript ss "$f" "$2" | cut -d' ' -f2)
busctl --user call org.kde.KWin "/Scripting/Script$id" org.kde.kwin.Script run
n=0
until out=$(journalctl --user -o cat --since="@$t" | grep -m1 -F "$2"); do
  n=$((n + 1))
  [ $n -lt 100 ] || break
  sleep 0.05
done
busctl --user call org.kde.KWin /Scripting org.kde.kwin.Scripting unloadScript s "$2" >/dev/null
rm -f "$f"
printf '%s\n' "${out#*"$2"}"
"#;

/// KWin's windows as the GNOME extension lists them.
const KWIN_LIST: &str = r#"const rect = r => [r.x, r.y, r.width, r.height];
const here = w => w.onAllDesktops || w.desktops.includes(workspace.currentDesktop);
console.info(MARK + JSON.stringify(workspace.stackingOrder.slice().reverse().map(w => ({
    id: w.internalId.toString(),
    pid: w.pid,
    title: w.caption,
    frame: rect(w.frameGeometry),
    client: rect(w.clientGeometry),
    popup: w.popupWindow,
    shown: !w.minimized && !w.hidden && here(w),
    focused: w === workspace.activeWindow,
    owner: w.transientFor ? w.transientFor.internalId.toString() : '',
}))));
"#;

/// KWin's window `ID` raised with the focus, as the GNOME extension's `Activate`.
const KWIN_RAISE: &str = r#"const w = workspace.stackingOrder.find(x => x.internalId.toString() === ID);
if (w) {
    w.minimized = false;
    workspace.activeWindow = w;
}
console.info(MARK + !!w);
"#;

/// Puts agent-desktop, its archive on stdin, in the guest.
const INSTALL: &str = "set -e
sudo -n mkdir -p /usr/local/bin
sudo -n tar -xzf - --no-same-owner -C /usr/local/bin agent-desktop
";

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
/// error when another window covers that point: a dialog, an always-on-top one, a popup.
pub fn click(
  name: &str,
  text: &str,
  app: Option<&str>,
  right: bool,
  double: bool,
) -> io::Result<()> {
  let vm = running(name, "click --name")?;
  let desktop = desktop(&vm)?;
  let (mut nodes, mut at) = element(&vm, text, app, None)?;

  // The menu bar, with no id, is in front of every window.
  let window = window_of(&nodes, &nodes[at]);
  let focused = |w: &Node| w.states.iter().any(|x| x == "focused");
  if !window.id.is_empty() && !focused(window) {
    // agent-desktop stops waiting for the focus at 750 ms, which macOS in a VM can pass.
    let id = window.id.clone();
    let raised = match desktop.wayland() {
      true => raise(&vm, desktop, &window.place.id),
      false => agent_desktop(&vm, &["focus-window", "--window-id", &id]).map(|_| ()),
    };

    if let Err(e) = raised
      && !windows(&vm)?.iter().any(|w| w.id == id && focused(w))
    {
      return Err(e);
    }

    // Read again, that window only: it may lay its elements out anew once it has the focus.
    (nodes, at) = element(&vm, text, app, Some(&id))?;
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
    let (active, all) = match desktop.wayland() {
      true => clients(&compositor(&vm, desktop)?),
      false => stack(&ssh::output(&vm, &format!("sh -c {}", ssh::quote(STACK)))?),
    };

    let like = |c: &&Client| c.window.name == window.name && c.window.frame == window.frame;
    // On Wayland, the compositor's window it was matched with.
    let mut twins: Vec<&Client> = match desktop.wayland() {
      true => all.iter().filter(|c| c.id == window.place.id).collect(),
      false => all.iter().filter(like).collect(),
    };

    if twins.is_empty() {
      twins = all.iter().filter(|c| c.window.name == window.name).collect();
    }

    let screen = if desktop.wayland() { "the compositor's" } else { "X's" };
    let own = |id: &str| twins.iter().any(|c| c.id == id) && (twins.len() == 1 || id == active);
    match all.iter().find(|c| c.window.covers(x, y)) {
      Some(c) if own(&c.id) => {}
      Some(c) if own(&c.owner) && in_menu(&nodes, node, c.window.frame) => {}
      Some(c) if twins.len() > 1 && twins.iter().any(|t| t.id == c.id) => {
        let line = window.line();
        return Err(bad(format!(
          "{line} has a twin of its title and frame, and {screen} focus is on neither"
        )));
      }
      Some(c) => return Err(bad(format!("{} is under {}", node.line(), c.window.line()))),
      None => return Err(bad(format!("{} is on none of {screen} windows", node.line()))),
    }
  }

  let (button, clicks) = (if right { "right" } else { "left" }, if double { 2 } else { 1 });
  vz::ask(&vm, &format!("click {x} {y} {button} {clicks}"), "")?;
  eprintln!("clicked {} at {x},{y}", node.line());
  Ok(())
}

/// The windows' elements, all apps' or one's, and where the one named `text` is among them, or
/// among the elements of the window with the id given: an error when none or more than one is.
fn element(
  vm: &Vm,
  text: &str,
  app: Option<&str>,
  window: Option<&str>,
) -> io::Result<(Vec<Node>, usize)> {
  let nodes = tree(vm, &windows(vm)?, app)?;
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

  Ok((nodes, at))
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

/// The VM's system's desktop.
fn desktop(vm: &Vm) -> io::Result<Desktop> {
  Ok(system::find(&vm.config.system)?.desktop)
}

/// Whether trivm puts agent-desktop in the VM's system: not in Windows, though it has a build for
/// it (UI Automation); nor in Ubuntu 22.04's Plasma: agent-desktop needs 24.04's AT-SPI, and found
/// no X window to raise for Konsole's.
fn supported(vm: &Vm) -> io::Result<bool> {
  Ok(match vm.config.os() {
    Os::Macos => true,
    Os::Linux => system::find(&vm.config.system)?.desktop != Desktop::Plasma,
    Os::Windows => false,
  })
}

/// `cmd` refused on a system agent-desktop isn't put in by trivm.
fn unsupported(vm: &Vm, cmd: &str) -> io::Error {
  bad(format!("{cmd}: not supported on {}", vm.config.system))
}

/// The visible windows, front to back, as agent-desktop lists them; on Wayland, where and in
/// which order the compositor shows them, with its focus: a toolkit there knows neither.
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

  let desktop = desktop(vm)?;
  if !desktop.wayland() {
    return Ok(all.items().iter().filter(shown).map(window).collect());
  }

  let screen = compositor(vm, desktop)?;
  let mut placed = Vec::new();
  for w in all.items().iter().filter(shown) {
    let mut node = window(w);
    let Some((at, s)) = matched(w, &screen) else {
      eprintln!("trivm: {}: can't tell which of the compositor's windows it is", node.line());
      continue;
    };

    // Minimized: a Wayland toolkit isn't told.
    if !s.shown {
      continue;
    }

    node.place = Place::new(w, s, &screen);
    node.frame = s.frame;
    node.states = s.focused.then(|| "focused".to_string()).into_iter().collect();
    placed.push((at, node));
  }

  placed.sort_by_key(|x| x.0);
  Ok(placed.into_iter().map(|x| x.1).collect())
}

/// A window the Wayland compositor has, as trivm's GNOME Shell extension and KWin script list
/// them.
struct Shown {
  id: String,
  pid: Option<f64>,
  title: String,
  /// Its frame on the screen: the compositor's title bar and borders in, a shadow out.
  frame: [f64; 4],
  /// Where its own coordinates are: inside the compositor's title bar and borders, the toolkit's
  /// own (GTK's) in.
  client: [f64; 4],
  /// A menu, a combo box's list, a tooltip.
  popup: bool,
  /// On the screen: not minimized, on the current workspace.
  shown: bool,
  focused: bool,
  /// The window it's for: a dialog's or popup's.
  owner: String,
}

impl Shown {
  fn parse(x: &Json) -> Self {
    let rect = |k: &str| -> [f64; 4] {
      let n: Vec<f64> = x.get(k).items().iter().filter_map(Json::num).collect();
      n.try_into().unwrap_or_default()
    };

    Shown {
      id: x.get("id").str().into(),
      pid: x.get("pid").num(),
      title: x.get("title").str().into(),
      frame: rect("frame"),
      client: rect("client"),
      popup: x.get("popup").bool(),
      shown: x.get("shown").bool(),
      focused: x.get("focused").bool(),
      owner: x.get("owner").str().into(),
    }
  }
}

/// The Wayland compositor's windows, top to bottom.
fn compositor(vm: &Vm, desktop: Desktop) -> io::Result<Vec<Shown>> {
  let text = match desktop {
    Desktop::GnomeWayland => gnome(vm, "List")?.str().to_string(),
    _ => kwin(vm, KWIN_LIST)?,
  };

  let Some(list) = Json::parse(&text) else {
    return Err(bad(format!("the compositor's windows in {}: {text:?}", vm.name)));
  };

  Ok(list.items().iter().map(Shown::parse).collect())
}

/// The window `id` of the Wayland compositor raised with the focus.
fn raise(vm: &Vm, desktop: Desktop, id: &str) -> io::Result<()> {
  let raised = match desktop {
    Desktop::GnomeWayland => gnome(vm, &format!("Activate t {}", ssh::quote(id)))?.bool(),
    _ => kwin(vm, &format!("const ID = {};\n{KWIN_RAISE}", json_str(id)))? == "true",
  };

  match raised {
    true => Ok(()),
    false => Err(bad(format!("the window {id} left {}", vm.name))),
  }
}

/// The answer of trivm's GNOME Shell extension, which the seed puts in (`system.rs`), to a call of
/// its method with arguments.
fn gnome(vm: &Vm, call: &str) -> io::Result<Json> {
  let line = format!("busctl --user --json=short call org.gnome.Shell /trivm trivm.Windows {call}");
  let out = ssh::output(vm, &line);
  let out = out.map_err(|e| bad(format!("trivm's GNOME Shell extension in {}: {e}", vm.name)))?;
  match Json::parse(&out) {
    Some(x) => Ok(x.get("data").items().first().cloned().unwrap_or_default()),
    None => Err(bad(format!("trivm's GNOME Shell extension in {}: {out:?}", vm.name))),
  }
}

/// What the KWin script logs, run with `MARK`, the mark its line starts with.
fn kwin(vm: &Vm, js: &str) -> io::Result<String> {
  let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |x| x.as_nanos());
  let mark = format!("trivm-{nanos} ");
  let js = format!("const MARK = {};\n{js}", json_str(&mark));
  let line = format!("sh -c {} kwin {} {}", ssh::quote(KWIN), ssh::quote(&js), ssh::quote(&mark));
  ssh::output(vm, &line)
}

/// The compositor's window agent-desktop's `w` is, and where it is in the stacking, told by its
/// process, then its title and size, else its title, its size, or as the process's only one; none
/// when twins leave it open. KWin titles Plasma's desktop and panel with nothing.
fn matched<'a>(w: &Json, screen: &'a [Shown]) -> Option<(usize, &'a Shown)> {
  let (pid, title) = (w.get("pid").num(), w.get("title").str().trim());
  let size = frame(w.get("bounds")).map(|[_, _, w, h]| (w, h));
  let near = |a: f64, b: f64| (a - b).abs() < 1.0;
  let titled = |s: &Shown| s.title.trim() == title;
  let sized = |s: &Shown| size.is_some_and(|(w, h)| near(w, s.client[2]) && near(h, s.client[3]));
  let tiers: [&dyn Fn(&Shown) -> bool; 4] =
    [&|s| titled(s) && sized(s), &titled, &sized, &|_| true];

  let mine: Vec<(usize, &Shown)> =
    screen.iter().enumerate().filter(|(_, s)| pid.is_some() && s.pid == pid && !s.popup).collect();
  tiers.iter().find_map(|f| match mine.iter().filter(|(_, s)| f(s)).collect::<Vec<_>>()[..] {
    [one] => Some(*one),
    _ => None,
  })
}

/// The compositor's focused window and the windows it shows, as `stack` gives X's.
fn clients(screen: &[Shown]) -> (String, Vec<Client>) {
  let active = screen.iter().find(|s| s.focused).map(|s| s.id.clone()).unwrap_or_default();
  (active, screen.iter().filter(|s| s.shown).map(Client::from).collect())
}

/// A window X or the Wayland compositor shows on a Linux guest's screen: a client, a popup, one
/// with no accessibility.
struct Client {
  id: String,
  /// The window it's for: a dialog's or popup's.
  owner: String,
  window: Node,
}

impl From<&Shown> for Client {
  fn from(s: &Shown) -> Self {
    let window =
      Node { role: "window".into(), name: s.title.clone(), frame: s.frame, ..Node::default() };
    Client { id: s.id.clone(), owner: s.owner.clone(), window }
  }
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
  let wanted = |x: &str| app.is_none_or(|a| x.eq_ignore_ascii_case(a));
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

    let ids: Vec<&str> = results.iter().map(|x| x.get("data").get("snapshot_id").str()).collect();
    let refmaps = match windows.iter().any(|w| w.place.origin.is_some()) {
      true => refmaps(vm, &ids)?,
      false => Json::Null,
    };

    for (i, (w, res)) in windows.iter().zip(results).enumerate() {
      let mut head = (*w).clone();
      head.place.refs = refmaps.items().get(i).map_or(Json::Null, |x| x.get("inner").clone());
      match res.get("error").get("message").str() {
        "" => add(&mut out, head, res.get("data")),
        msg => eprintln!("trivm: {}: {msg}", w.line()),
      }
    }
  }

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

/// A window's or menu bar's line, then the elements of its snapshot.
fn add(out: &mut Vec<Node>, mut head: Node, snapshot: &Json) {
  if !snapshot.get("complete").bool() {
    eprintln!("trivm: {}: its tree was cut short", head.line());
  }

  let mut place = head.place.clone();
  place.refs = mem::take(&mut head.place.refs);
  out.push(head);
  for x in snapshot.get("tree").get("children").items() {
    flatten(x, 1, out, &place);
  }
}

/// The refmaps of agent-desktop's snapshots, in order: null for one it has none of.
fn refmaps(vm: &Vm, ids: &[&str]) -> io::Result<Json> {
  let ids: Vec<String> = ids.iter().map(|x| ssh::quote(x)).collect();
  let out = ssh::output(vm, &format!("sh -c {} refmaps {}", ssh::quote(REFMAPS), ids.join(" ")))?;
  Json::parse(&out).ok_or_else(|| bad(format!("agent-desktop's refmaps in {}", vm.name)))
}

/// agent-desktop's answer to the arguments in the running guest, its `data`. It is installed
/// first when it isn't there, and on macOS granted Accessibility when it lacks it.
fn agent_desktop(vm: &Vm, args: &[&str]) -> io::Result<Json> {
  let (script, rest) = match vm.config.os() {
    Os::Macos => (format!("{HAS}{ALLOWED}{MACOS_RUN}"), ssh::quote(&plist_strings(args))),
    Os::Linux => {
      let args: Vec<_> = args.iter().map(|x| ssh::quote(x)).collect();
      let env = if desktop(vm)?.wayland() { WAYLAND_ENV } else { "" };
      (format!("{HAS}shift\n{env}exec {BIN} \"$@\"\n"), args.join(" "))
    }
    Os::Windows => return Err(unsupported(vm, "agent-desktop")),
  };

  let line = format!("sh -c {} ui {VERSION} {rest}", ssh::quote(&script));
  let (mut installed, mut granted) = (false, false);
  let out = loop {
    let out = ssh::ssh(vm)?.arg("--").arg(&line).stdin(Stdio::null()).output()?;
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

/// Puts agent-desktop in the guest from trivm's cache; on Linux switches accessibility on.
fn install(vm: &Vm) -> io::Result<()> {
  let ((url, sha256), script) = match vm.config.os() {
    Os::Macos => (MACOS, INSTALL.to_string()),
    Os::Linux => (LINUX, format!("{INSTALL}{A11Y}")),
    Os::Windows => return Err(unsupported(vm, "agent-desktop")),
  };

  let archive = system::download(url, sha256)?;
  eprintln!("installing agent-desktop {VERSION} in {}", vm.name);

  run(ssh::ssh(vm)?.arg("--").arg(script).stdin(fs::File::open(&archive)?))?;
  Ok(())
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
  /// A window's on Wayland.
  place: Place,
}

/// Where a window on Wayland is, for its elements' frames: a toolkit there gives them in the
/// window's own coordinates.
#[derive(Clone, Debug, Default, PartialEq)]
struct Place {
  /// The compositor's id of the window.
  id: String,
  /// Added to a frame in the snapshot.
  shift: [f64; 2],
  /// Where GTK 4's window coordinates start on the screen, for a frame in the refmap: the
  /// window's frame, its shadow out.
  origin: Option<[f64; 2]>,
  /// The window's popup, where its open menu is.
  popup: Option<[f64; 4]>,
  /// agent-desktop's refmap of its snapshot, its `inner`.
  refs: Json,
}

impl Place {
  /// Where agent-desktop's window `w`, the compositor's `s`, puts its elements on the screen.
  fn new(w: &Json, s: &Shown, screen: &[Shown]) -> Self {
    // A toolkit that knows where its window is, an X11 one, gives frames on the screen; a Wayland
    // one in its window's coordinates, the window at 0,0, or none (GTK 4).
    let ([x, y, ..], [l, t, ..]) = (s.client, frame(w.get("bounds")).unwrap_or_default());
    let popup = screen.iter().find(|p| p.popup && p.shown && p.owner == s.id).map(|p| p.frame);
    let origin = Some([s.frame[0], s.frame[1]]);
    Place { id: s.id.clone(), shift: [x - l, y - t], origin, popup, ..Place::default() }
  }

  /// An element's frame on the screen: the one in its window's coordinates that agent-desktop
  /// keeps for its ref, GTK 4's, from the window's frame (on Xwayland agent-desktop counts it
  /// from the shadow), else its bounds in the snapshot.
  fn frame(&self, node: &Json) -> Option<[f64; 4]> {
    let id = node.get("ref_id").str().rsplit([':', '@']).next().unwrap_or_default();
    let entry = self.refs.get(&format!("@{id}"));
    let window = frame(entry.get("bounds")).filter(|_| entry.get("bounds_space").str() == "window");
    if let (Some([left, top]), Some([x, y, w, h])) = (self.origin, window) {
      return Some([x + left, y + top, w, h]);
    }

    let [x, y, w, h] = frame(node.get("bounds"))?;
    Some([x + self.shift[0], y + self.shift[1], w, h])
  }
}

impl Node {
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
fn flatten(node: &Json, depth: usize, out: &mut Vec<Node>, place: &Place) {
  let states: Vec<String> = node.get("states").items().iter().map(|x| x.str().into()).collect();
  if states.iter().any(|x| x == "hidden" || x == "offscreen") {
    return;
  }

  let (role, name) = (node.get("role").str(), node.get("name").str());
  let children = node.get("children").items();
  let Some(frame) = place.frame(node) else {
    // A closed menu's custom views (Finder's tags) have frames at the screen's corner. On Wayland
    // an open one has no frame either.
    if role == "menu" {
      menu(node, depth, out, place);
      return;
    }

    for x in children {
      flatten(x, depth, out, place);
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
    flatten(x, depth + 1, out, place);
  }

  if nameless && out.len() == at + 1 {
    out.pop();
  }
}

/// A menu with no frame on Wayland, open when its window has a popup and the menu items: GTK 4
/// lays them out in the window's coordinates, but the compositor puts the popup where it fits on
/// the screen, so they are centered in it, the menu's frame.
fn menu(node: &Json, depth: usize, out: &mut Vec<Node>, place: &Place) {
  let Some(popup) = place.popup else { return };
  let (at, name) = (out.len(), node.get("name").str().into());
  out.push(Node { depth, role: "menu".into(), name, frame: popup, ..Node::default() });
  for x in node.get("children").items() {
    flatten(x, depth + 1, out, place);
  }

  let items = &mut out[at + 1..];
  if items.is_empty() {
    out.pop();
    return;
  }

  let (mut left, mut top, mut right, mut bottom) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
  for item in items.iter() {
    let [x, y, w, h] = item.frame;
    (left, top, right, bottom) = (left.min(x), top.min(y), right.max(x + w), bottom.max(y + h));
  }

  let [x, y, w, h] = popup;
  let (dx, dy) = (x + (w - right + left) / 2.0 - left, y + (h - bottom + top) / 2.0 - top);
  for item in items {
    item.frame[0] += dx;
    item.frame[1] += dy;
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

/// The window or menu bar an element is in: the last line at depth 0 up to it.
fn window_of<'a>(nodes: &'a [Node], node: &'a Node) -> &'a Node {
  let at = nodes.iter().position(|x| std::ptr::eq(x, node)).unwrap_or(0);
  nodes[..=at].iter().rev().find(|x| x.depth == 0).unwrap_or(node)
}

// MARK: JSON

/// A JSON value, as agent-desktop prints them.
#[derive(Clone, Debug, Default, PartialEq)]
enum Json {
  #[default]
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
      flatten(x, 1, &mut out, &Place::default());
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
      place: Place::default(),
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

  /// What KWin's script lists on Plasma: its panel, Konsole on X11 over Konsole on Wayland, the
  /// desktop; and what agent-desktop lists of them, a Wayland window at 0,0.
  const KWIN_WINDOWS: &str = r#"[
    {"id":"{c3}","pid":23388,"title":"","frame":[0,1018,1920,62],"client":[0,1018,1920,62],"popup":false,"shown":true,"focused":false,"owner":""},
    {"id":"{55}","pid":25559,"title":"~ : bash — Konsole","frame":[544,291,911,496],"client":[544,319,911,468],"popup":false,"shown":true,"focused":true,"owner":""},
    {"id":"{85}","pid":24092,"title":"tmp : bash — Konsole","frame":[504,269,911,496],"client":[504,297,911,468],"popup":false,"shown":true,"focused":false,"owner":""},
    {"id":"{d8}","pid":23388,"title":"","frame":[0,0,1920,1080],"client":[0,0,1920,1080],"popup":false,"shown":true,"focused":false,"owner":""}
  ]"#;
  const AGENT_WINDOWS: &str = r#"[
    {"pid":24092,"title":"tmp : bash — Konsole","bounds":{"height":468.0,"width":911.0,"x":0.0,"y":0.0}},
    {"pid":23388,"title":"Desktop @ QRect(0,0 1920x1080) ","bounds":{"height":1080.0,"width":1920.0,"x":0.0,"y":0.0}},
    {"pid":23388,"title":"","bounds":{"height":62.0,"width":1920.0,"x":0.0,"y":1018.0}},
    {"pid":25559,"title":"~ : bash — Konsole","bounds":{"height":468.0,"width":911.0,"x":544.0,"y":319.0}},
    {"pid":99,"title":"gone"}
  ]"#;

  #[test]
  fn wayland_windows() {
    let screen: Vec<Shown> =
      Json::parse(KWIN_WINDOWS).unwrap().items().iter().map(Shown::parse).collect();
    let agent = Json::parse(AGENT_WINDOWS).unwrap();
    let found: Vec<Option<(usize, [f64; 2])>> = agent
      .items()
      .iter()
      .map(|w| matched(w, &screen).map(|(at, s)| (at, Place::new(w, s, &screen).shift)))
      .collect();
    // By title, by size (KWin titles Plasma's windows with nothing), none; X11's Konsole already
    // on the screen.
    assert_eq!(
      found,
      [
        Some((2, [504.0, 297.0])),
        Some((3, [0.0, 0.0])),
        Some((0, [0.0, 0.0])),
        Some((1, [0.0, 0.0])),
        None
      ]
    );

    // Twins of a process, title and size: neither is told.
    let mut twins = Json::parse(KWIN_WINDOWS).unwrap();
    if let Json::Arr(x) = &mut twins {
      x[1] = x[2].clone();
      if let Json::Obj(f) = &mut x[1] {
        f[0].1 = Json::Str("{86}".into());
      }
    }

    let twins: Vec<Shown> = twins.items().iter().map(Shown::parse).collect();
    assert!(matched(&agent.items()[0], &twins).is_none());

    let (active, all) = clients(&screen);
    assert_eq!(active, "{55}");
    let lines: Vec<String> = all.iter().map(|c| format!("{} {}", c.id, c.window.line())).collect();
    assert_eq!(lines[1], r#"{55} window "~ : bash — Konsole" 544,291 911x496"#);
    assert_eq!(lines.len(), 4);
  }

  /// GTK 4's Files on Wayland, cut down: its window and open main menu as trivm's GNOME Shell
  /// extension lists them, its snapshot with no bounds and its refmap.
  const GNOME_WINDOWS: &str = r#"[
    {"id":"31","pid":24270,"title":"","frame":[596,321,219,308],"client":[596,321,219,308],"popup":true,"shown":true,"focused":false,"owner":"28"},
    {"id":"28","pid":24270,"title":"Recent","frame":[548,281,890,550],"client":[548,281,890,550],"popup":false,"shown":true,"focused":true,"owner":""}
  ]"#;
  const FILES: &str = r#"{"children":[
    {"role":"button","name":"Back","ref_id":"@s1:e1"},
    {"role":"statictext","name":"Recent"},
    {"role":"button","name":"Main Menu","ref_id":"@s1:e2","children":[
      {"role":"menu","name":"Main Menu","children":[
        {"role":"menuitem","name":"New Window","ref_id":"@s1:e3"},
        {"role":"group","children":[
          {"role":"menuitem","name":"About Files","ref_id":"@s1:e4"}]}]}]},
    {"role":"button","name":"View Options","ref_id":"@s1:e5","children":[
      {"role":"menu","name":"View","children":[
        {"role":"menuitem","name":"Icons","ref_id":"@s1:e6","states":["hidden"]}]}]}
  ]}"#;
  const REFMAP: &str = r#"{
    "@e1":{"bounds":{"x":179.0,"y":0.0,"width":34.0,"height":34.0},"bounds_space":"window"},
    "@e2":{"bounds":{"x":133.0,"y":0.0,"width":34.0,"height":34.0},"bounds_space":"window"},
    "@e3":{"bounds":{"x":66.0,"y":71.0,"width":205.0,"height":32.0},"bounds_space":"window"},
    "@e4":{"bounds":{"x":66.0,"y":321.0,"width":205.0,"height":32.0},"bounds_space":"window"},
    "@e5":{"bounds":{"x":706.0,"y":0.0,"width":34.0,"height":34.0}}
  }"#;

  #[test]
  fn wayland_frames() {
    let screen: Vec<Shown> =
      Json::parse(GNOME_WINDOWS).unwrap().items().iter().map(Shown::parse).collect();
    let w = Json::parse(r#"{"pid":24270,"title":"Recent"}"#).unwrap();
    let (at, s) = matched(&w, &screen).unwrap();
    let mut place = Place::new(&w, s, &screen);
    assert_eq!((at, place.shift, place.origin), (1, [548.0, 281.0], Some([548.0, 281.0])));
    assert_eq!(place.popup, Some([596.0, 321.0, 219.0, 308.0]));
    place.refs = Json::parse(REFMAP).unwrap();

    let mut out = Vec::new();
    for x in Json::parse(FILES).unwrap().get("children").items() {
      flatten(x, 1, &mut out, &place);
    }

    // A frame in screen space only from the snapshot; the open menu's items centered in the
    // popup, the closed one's left out.
    let lines: Vec<String> = out.iter().map(|x| x.to_string()).collect();
    assert_eq!(
      lines,
      [
        r#"  button "Back" 727,281 34x34"#,
        r#"  button "Main Menu" 681,281 34x34"#,
        r#"    menu "Main Menu" 596,321 219x308"#,
        r#"      menuitem "New Window" 603,334 205x32"#,
        r#"      menuitem "About Files" 603,584 205x32"#,
      ]
    );

    // No popup: the menu is closed.
    place.popup = None;
    let mut out = Vec::new();
    flatten(&Json::parse(FILES).unwrap().get("children").items()[2], 1, &mut out, &place);
    assert_eq!(out.len(), 1);
  }

  #[test]
  fn plist() {
    assert_eq!(
      plist_strings(&["batch", r#"[{"a":"<&>"}]"#]),
      r#"<string>batch</string><string>[{"a":"&lt;&amp;&gt;"}]</string>"#
    );
  }
}
