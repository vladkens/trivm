// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! Permissions for the app under test in a macOS guest: SIP turned off (or on) from recoveryOS,
//! whose screens trivm reads with Vision's text recognition and drives from the host, and the TCC
//! grants written for an app into the guest's databases, which root may do with SIP off.

use std::path::Path;
use std::time::{Duration, Instant};
use std::{env, fs, io, thread};

use crate::vm::{Os, PASSWORD, Vm};
use crate::{bad, ssh, vision, vz};

/// A macOS VM that is set up: SIP and TCC are macOS's.
fn macos(name: &str) -> io::Result<Vm> {
  let vm = Vm::open(name)?;
  if vm.config.os() != Os::Macos {
    return Err(bad(format!("{name} isn't macOS: SIP and TCC grants are macOS's")));
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
  let screen = |text: &str, secs: u64| vision::wait_text(vm, shot, text, Duration::from_secs(secs));

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

// MARK: TCC grants

/// A service `grant` takes: its name, TCC's services for it, and whether the user's database keeps
/// them, not the system's.
pub struct Service {
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

/// Prints SIP's state, then the app's TCC client: a bundle by its identifier (type 0), which is
/// what tccd looks a bundle's executable up by, anything else by its path (type 1).
const APP: &str = r#"set -e
csrutil status
if [ -f "$1/Contents/Info.plist" ]; then
  echo 0
  /usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$1/Contents/Info.plist"
elif [ -f "$1" ]; then
  echo 1
  echo "$1"
else
  echo "no app at $1" >&2
  exit 1
fi
"#;

/// Runs `$1` on the system's TCC.db and `$2`, if any, on the user's (macOS 27's is in a container
/// of tccd's, older ones' in `~/Library`), then kills tccd, which reads them once and ignores
/// SIGTERM; launchd starts it again, on demand, so none may be running.
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
sudo -n killall -9 tccd 2>/dev/null || true
"#;

/// `grant <vm> <app> [<service>...]`: the services, all by default, allowed for the app at a
/// path in the running guest, which needs SIP off.
pub fn grant(name: &str, app: &str, names: &[String]) -> io::Result<()> {
  let vm = macos(name)?;
  let services = services(names)?;
  let client = allow(&vm, app, &services)?;
  let names: Vec<_> = services.iter().map(|s| s.name).collect();
  eprintln!(
    "{client} may use {} in {name}; an app that runs sees them once relaunched",
    names.join(", ")
  );
  Ok(())
}

/// Writes the grants of the services for the app at a path in a running macOS guest with SIP
/// off; the app's TCC client, its bundle identifier or path.
pub fn allow(vm: &Vm, app: &str, services: &[&Service]) -> io::Result<String> {
  let name = &vm.name;
  if !app.starts_with('/') {
    return Err(bad(format!("{app}: the app's path in the guest, from /")));
  }

  if vm.pid().is_none() {
    return Err(bad(format!("{name} isn't running; trivm start {name}")));
  }

  let out = ssh::output(vm, &format!("sh -c {} app {}", ssh::quote(APP), ssh::quote(app)))?;
  let lines: Vec<&str> = out.lines().collect();
  let [status, kind, client] = lines[..] else {
    return Err(bad(format!("{app} in {name}: {out}")));
  };

  if !status.contains("status: disabled") {
    return Err(bad(format!(
      "SIP is on in {name}, which keeps TCC's databases from root; trivm sip {name} off"
    )));
  }

  let kind: u8 = kind.parse().map_err(|_| bad(format!("{app} in {name}: {out}")))?;
  let pick = |user: bool| -> Vec<&str> {
    services.iter().filter(|s| s.user == user).flat_map(|s| s.tcc.iter().copied()).collect()
  };
  let (system, user) = (sql(&pick(false), client, kind), sql(&pick(true), client, kind));
  let script =
    format!("sh -c {} write {} {}", ssh::quote(WRITE), ssh::quote(&system), ssh::quote(&user));
  ssh::output(vm, &script)?;
  Ok(client.to_string())
}

/// The services named, all with none.
pub fn services(names: &[String]) -> io::Result<Vec<&'static Service>> {
  let all: Vec<_> = SERVICES.iter().map(|s| s.name).collect();
  let find = |x: &str| {
    let known = SERVICES.iter().find(|s| s.name == x);
    known.ok_or_else(|| bad(format!("unknown service {x}; {}", all.join(", "))))
  };
  match names {
    [] => Ok(SERVICES.iter().collect()),
    _ => names.iter().map(|x| find(x)).collect(),
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
