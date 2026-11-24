// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! The guest's desktop as named elements: agent-desktop, which trivm puts in the guest, reads its
//! windows' accessibility trees (AX on macOS, AT-SPI on Linux); `ui` prints them a line an
//! element, with its role, name and frame in a shot's pixels, and `click --name` clicks one.

use std::process::Stdio;
use std::{fmt, fs, io};

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

/// Exits 4 unless agent-desktop is allowed Accessibility. Run without it, it would have macOS put
/// up its prompt to allow it on the guest's screen at its first call to AX; with SIP on, root
/// can't read TCC's database.
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
  let nodes = tree(&running(name)?, app)?;
  if nodes.is_empty() {
    eprintln!("no windows on {name}'s desktop");
  }

  for x in nodes {
    println!("{x}");
  }

  Ok(())
}

/// `click <vm> --name <text>`: the element of that name clicked at its frame's middle, as
/// `click <x> <y>` does.
pub fn click(
  name: &str,
  text: &str,
  app: Option<&str>,
  right: bool,
  double: bool,
) -> io::Result<()> {
  let vm = running(name)?;
  let nodes = tree(&vm, app)?;
  let node = match find(&nodes, text)[..] {
    [one] => one,
    [] => return Err(bad(format!("no element named {text:?} in {name}"))),
    ref found => {
      let lines: Vec<String> = found.iter().map(|x| format!("\n  {}", x.line())).collect();
      let (n, lines) = (found.len(), lines.concat());
      return Err(bad(format!(
        "{n} elements named {text:?} in {name}; click one by its frame:{lines}"
      )));
    }
  };

  let (x, y) = node.middle();
  let screen = vz::screen(vm.config.os());
  if !(0.0..screen.width).contains(&x) || !(0.0..screen.height).contains(&y) {
    return Err(bad(format!("{} is off the screen", node.line())));
  }

  let (button, clicks) = (if right { "right" } else { "left" }, if double { 2 } else { 1 });
  vz::ask(&vm, &format!("click {x} {y} {button} {clicks}"), "")?;
  eprintln!("clicked {} at {x},{y}", node.line());
  Ok(())
}

fn running(name: &str) -> io::Result<Vm> {
  let vm = Vm::open(name)?;
  match vm.pid() {
    Some(_) => Ok(vm),
    None => Err(bad(format!("{name} isn't running; trivm start {name}"))),
  }
}

/// The visible windows, all apps' or one's, each followed by its elements, after the menu bar
/// on macOS, the frontmost app's: agent-desktop lists the windows, then reads their trees in one
/// batch.
fn tree(vm: &Vm, app: Option<&str>) -> io::Result<Vec<Node>> {
  let wanted = |x: &str| app.is_none_or(|a| x.eq_ignore_ascii_case(a));
  let all = agent_desktop(vm, &["list-windows"])?;
  let shown =
    |w: &&Json| w.get("visible").bool() && w.get("accessible").bool() && !w.get("minimized").bool();
  let shown: Vec<&Json> = all.items().iter().filter(shown).collect();
  let windows: Vec<&Json> =
    shown.iter().copied().filter(|w| wanted(w.get("app_name").str())).collect();

  let snapshot = |scope: String| {
    format!(
      r#"{{"command":"snapshot","args":{{{scope},"include_bounds":true,"compact":true,"max_depth":{DEPTH}}}}}"#
    )
  };
  let window = |w: &&Json| snapshot(format!(r#""window_id":{}"#, json_str(w.get("id").str())));
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
      let states = Vec::new();
      let (app, role, name, value) = (front.into(), "menubar".into(), String::new(), String::new());
      add(&mut out, Node { depth: 0, app, role, name, value, states, frame }, bar);
    }

    for (w, res) in windows.iter().zip(results) {
      let window = Node {
        depth: 0,
        app: w.get("app_name").str().into(),
        role: "window".into(),
        name: w.get("title").str().into(),
        value: String::new(),
        states: w.get("is_focused").bool().then(|| "focused".to_string()).into_iter().collect(),
        frame: frame(w.get("bounds")).unwrap_or_default(),
      };

      match res.get("error").get("message").str() {
        "" => add(&mut out, window, res.get("data")),
        msg => eprintln!("trivm: {}: {msg}", window.line()),
      }
    }
  }

  if let Some(app) = app
    && out.is_empty()
  {
    let mut apps: Vec<&str> = shown.iter().map(|w| w.get("app_name").str()).collect();
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
  let os = vm.config.os();
  let script = match os {
    Os::Macos => format!("{HAS}{ALLOWED}{MACOS_RUN}"),
    Os::Linux => format!("{HAS}shift\nexec {BIN} \"$@\"\n"),
  };

  let rest = match os {
    Os::Macos => ssh::quote(&plist_strings(args)),
    Os::Linux => args.iter().map(|x| ssh::quote(x)).collect::<Vec<_>>().join(" "),
  };

  let line = format!("sh -c {} ui {VERSION} {rest}", ssh::quote(&script));
  let mut fixed = false;
  let out = loop {
    let out = ssh::ssh(vm)?.arg("--").arg(&line).stdin(Stdio::null()).output()?;
    match out.status.code() {
      Some(3) if !fixed => install(vm)?,
      Some(4) if !fixed => grant(vm)?,
      _ => break out,
    }

    fixed = true;
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

/// Puts agent-desktop in the guest from trivm's cache; on Linux switches accessibility on, on
/// macOS grants it Accessibility.
fn install(vm: &Vm) -> io::Result<()> {
  let os = vm.config.os();
  let (url, sha256) = match os {
    Os::Macos => MACOS,
    Os::Linux => LINUX,
  };

  let archive = system::download(url, sha256)?;
  eprintln!("installing agent-desktop {VERSION} in {}", vm.name);
  let script = match os {
    Os::Macos => INSTALL.to_string(),
    Os::Linux => format!("{INSTALL}{A11Y}"),
  };

  run(ssh::ssh(vm)?.arg("--").arg(script).stdin(fs::File::open(&archive)?))?;
  match os {
    Os::Macos => grant(vm),
    Os::Linux => Ok(()),
  }
}

/// Accessibility for agent-desktop by its path, as `grant` gives it, which needs SIP off.
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
#[derive(Debug, PartialEq)]
struct Node {
  depth: usize,
  /// The app, a window's.
  app: String,
  role: String,
  name: String,
  value: String,
  states: Vec<String>,
  /// x, y, width and height in a shot's pixels.
  frame: [f64; 4],
}

impl Node {
  /// The middle of its frame, where a click goes.
  fn middle(&self) -> (f64, f64) {
    let [x, y, w, h] = self.frame;
    ((x + w / 2.0).round(), (y + h / 2.0).round())
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
/// not its children, a hidden one with its children, and a nameless leaf with no value.
fn flatten(node: &Json, depth: usize, out: &mut Vec<Node>) {
  let states: Vec<String> = node.get("states").items().iter().map(|x| x.str().into()).collect();
  if states.iter().any(|x| x == "hidden" || x == "offscreen") {
    return;
  }

  let children = node.get("children").items();
  let Some(frame) = frame(node.get("bounds")) else {
    for x in children {
      flatten(x, depth, out);
    }

    return;
  };

  let (role, name) = (node.get("role").str(), node.get("name").str());
  // A toggle's value is its state, which `checked` says.
  let value = match node.get("value").str() {
    x if x == name || ["checkbox", "switch", "radiobutton"].contains(&role) => "",
    x => x,
  };

  let at = out.len();
  let (role, name, value) = (role.into(), name.to_string(), value.to_string());
  let nameless = name.is_empty() && value.is_empty();
  out.push(Node { depth, app: String::new(), role, name, value, states, frame });
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

/// The elements whose name is `text`, else those whose name holds it, ignoring case; windows
/// are not elements to click.
fn find<'a>(nodes: &'a [Node], text: &str) -> Vec<&'a Node> {
  let elements = || nodes.iter().filter(|x| x.depth > 0);
  let found: Vec<&Node> = elements().filter(|x| x.name == text).collect();
  if !found.is_empty() {
    return found;
  }

  let low = text.to_lowercase();
  elements().filter(|x| x.name.to_lowercase().contains(&low)).collect()
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
  /// hidden element, a nameless group, a nameless leaf, and a group with no frame.
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
    {"bounds":{"height":0.0,"width":10.0,"x":5.0,"y":81.0},"name":"Flat","role":"button"}
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
      app: "Wavo".into(),
      role: "window".into(),
      name: "a \"b\"\n".into(),
      value: String::new(),
      states: vec!["focused".into()],
      frame: [520.0, 100.0, 560.0, 593.0],
    };
    assert_eq!(window.to_string(), r#"Wavo: window "a \"b\"\n" [focused] 520,100 560x593"#);
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
  }

  #[test]
  fn plist() {
    assert_eq!(
      plist_strings(&["batch", r#"[{"a":"<&>"}]"#]),
      r#"<string>batch</string><string>[{"a":"&lt;&amp;&gt;"}]</string>"#
    );
  }
}
