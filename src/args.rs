// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! The command line: trivm's commands and their arguments, parsed by hand, and the help.

use std::io;

use crate::bad;

pub const HELP: &str = "\
trivm: macOS, Linux and Windows VMs on Apple silicon for coding agents to test apps in.

Usage: trivm <command> [args]

VMs:
  create <system> [--name <vm>] [--cpus <n>] [--memory <GiB>] [--disk <GiB>]
                          install a system unattended and start it; systems: macos-27,
                          macos-26, macos-15, ubuntu-24.04, ubuntu-22.04, debian-13, fedora-45,
                          windows-11 (needs brew install qemu swtpm wimlib)
  start <vm>              start in the background, wait for the desktop
  stop <vm> [--force]     shut down; --force stops at once
  list                    the VMs, their systems and states
  delete <vm>             stop and remove
  open <vm>               show the VM's window, to look or use the mouse

Clones of a stopped VM, made in a second with APFS and sharing its disk until written:
  clone <vm> <name>       a clone to keep, with its own address
  run <vm> [--] <cmd>...  run a command on the host with a new clone started and its name in
                          TRIVM_VM, then delete the clone; exit with the command's code. One
                          argument is a shell line, more are run as they are

The guest system's own integrations, macOS's so far (not supported on Linux or Windows):
  os grant <vm> <app> [<service>...]
                          allow an app, by its path in the guest, the services, all by default:
                          accessibility, screen (recording), input (monitoring), microphone;
                          turns SIP off first if it's on. A running app sees them once relaunched
  os sip <vm> on|off      turn System Integrity Protection on or off from recoveryOS, which trivm
                          drives (about a minute); the VM is up after

Commands and files, through SSH:
  exec <vm> [--] <cmd>... run a command, exit with its code; one argument is a shell line, more
                          are passed as they are; on Windows PowerShell 5.1, which drops a
                          program's empty arguments and the double quotes in them, and where
                          `desktop <program>` starts a program on the desktop
  ssh <vm>                an interactive shell
  cp <src> <dst>          copy files, `<vm>:<path>` on either side, folders too
  ip <vm>                 the guest's address

The screen, from the host:
  shot <vm> [file]        a PNG of the screen, to `file` or a temporary file, its path printed
  click <vm> <x> <y> [--right] [--double]
                          click at a point in the shot's pixels
  move <vm> <x> <y>       move the pointer there
  type <vm> <text>        type text (US layout)
  key <vm> <keys>         press keys together, e.g. enter, cmd+space, ctrl+alt+t

A microphone, the guest's default input, which trivm puts in the guest at the first use
(BlackHole on macOS), fed from the host, never from the Mac's microphone:
  mic <vm> [file]         play an audio file (WAV, MP3, AAC, FLAC...) into it in real time,
                          return when played; with no file, only put it there

The desktop's elements, read by agent-desktop, which trivm puts in the guest at the first use
(on macOS with its Accessibility grant, which needs SIP off; not on ubuntu-22.04 or Windows):
  ui <vm> [--app <name>]  the visible windows, all apps' or one's, and macOS's menu bar, with
                          their elements, a line each: role, \"name\", = \"value\", [states] and
                          frame (x,y wxh in a shot's pixels)
  click <vm> --name <text> [--app <name>] [--right] [--double]
                          click the element of that name, else of it in any case, else the one
                          whose name holds it, its window raised first

The guest's user is `user`, password `1234`, sudo without one (an administrator on Windows).
Files, by the XDG Base Directory spec: each VM's settings in ~/.config/trivm/<vm>.conf
(XDG_CONFIG_HOME), its disks in ~/.local/share/trivm (XDG_DATA_HOME), downloads in
~/.cache/trivm (XDG_CACHE_HOME), logs, locks and sockets in ~/.local/state/trivm
(XDG_STATE_HOME).
";

#[derive(Debug, PartialEq)]
pub enum Cmd {
  Help,
  Version,
  Create {
    system: String,
    name: Option<String>,
    cpus: Option<u32>,
    memory: Option<u32>,
    disk: Option<u32>,
  },
  Start {
    vm: String,
  },
  Stop {
    vm: String,
    force: bool,
  },
  List,
  Delete {
    vm: String,
  },
  Open {
    vm: String,
  },
  Exec {
    vm: String,
    cmd: Vec<String>,
  },
  Ssh {
    vm: String,
  },
  Cp {
    src: String,
    dst: String,
  },
  Ip {
    vm: String,
  },
  Shot {
    vm: String,
    file: Option<String>,
  },
  Click {
    vm: String,
    x: f64,
    y: f64,
    right: bool,
    double: bool,
  },
  Move {
    vm: String,
    x: f64,
    y: f64,
  },
  Type {
    vm: String,
    text: String,
  },
  Key {
    vm: String,
    keys: String,
  },
  Mic {
    vm: String,
    file: Option<String>,
  },
  Ui {
    vm: String,
    app: Option<String>,
  },
  ClickName {
    vm: String,
    name: String,
    app: Option<String>,
    right: bool,
    double: bool,
  },
  Clone {
    vm: String,
    name: String,
  },
  Run {
    vm: String,
    cmd: Vec<String>,
  },
  /// `os sip`.
  Sip {
    vm: String,
    on: bool,
  },
  /// `os grant`.
  Grant {
    vm: String,
    app: String,
    services: Vec<String>,
  },
  /// The process that runs a VM, started by `start`; macOS's from recoveryOS for `os sip`.
  Serve {
    vm: String,
    recovery: bool,
  },
  /// The process that deletes a `run`'s clone if `run` ends without doing it.
  Reap,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> io::Result<Cmd> {
  let mut args = args.into_iter();
  let Some(cmd) = args.next() else { return Ok(Cmd::Help) };
  let rest: Vec<String> = args.collect();

  // `os` holds the guest system's own integrations, each a command of its own: `os grant`.
  let (cmd, rest) = match rest.split_first() {
    Some((sub, rest)) if cmd == "os" && !sub.starts_with('-') => {
      (format!("os {sub}"), rest.to_vec())
    }
    _ => (cmd, rest),
  };

  if cmd == "exec" || cmd == "run" {
    let mut rest = rest.into_iter();
    let vm = rest.next().ok_or_else(|| bad(format!("{cmd}: no VM given")))?;
    let mut line: Vec<String> = rest.collect();
    if line.first().is_some_and(|x| x == "--") {
      line.remove(0);
    }

    if line.is_empty() {
      return Err(bad(format!("{cmd}: no command given")));
    }

    return Ok(match cmd.as_str() {
      "exec" => Cmd::Exec { vm, cmd: line },
      _ => Cmd::Run { vm, cmd: line },
    });
  }

  let mut pos = Vec::new();
  let mut flags = Vec::new();
  let mut it = rest.into_iter();
  while let Some(x) = it.next() {
    match x.as_str() {
      // `type` takes any text, dashes included.
      _ if cmd == "type" => pos.push(x),
      "--name" | "--app" | "--cpus" | "--memory" | "--disk" => {
        let value = it.next().ok_or_else(|| bad(format!("{x}: no value given")))?;
        flags.push((x, Some(value)));
      }
      "--force" | "--right" | "--double" | "--recovery" | "-h" | "--help" => flags.push((x, None)),
      _ if x.starts_with("--") => return Err(bad(format!("unknown flag {x}"))),
      _ => pos.push(x),
    }
  }

  let help = |x: &str| ["help", "-h", "--help"].contains(&x);
  if help(&cmd) || flags.iter().any(|(x, _)| help(x)) {
    return Ok(Cmd::Help);
  }

  let flag = |name: &str| flags.iter().any(|(x, _)| x == name);
  let value = |name: &str| flags.iter().find(|(x, _)| x == name).and_then(|(_, v)| v.clone());
  let number = |name: &str| -> io::Result<Option<u32>> {
    let Some(x) = value(name) else { return Ok(None) };
    x.parse().map(Some).map_err(|_| bad(format!("{name}: not a number: {x}")))
  };
  let coord = |x: &str| -> io::Result<f64> {
    x.parse().ok().filter(|x: &f64| *x >= 0.0).ok_or_else(|| bad(format!("not a point: {x}")))
  };

  let want = |n: usize, usage: &str| -> io::Result<()> {
    match pos.len() == n {
      true => Ok(()),
      false => Err(bad(format!("usage: trivm {cmd} {usage}"))),
    }
  };

  let cmd = match cmd.as_str() {
    "-V" | "--version" | "version" => Cmd::Version,
    "create" => {
      want(1, "<system> [--name <vm>] [--cpus <n>] [--memory <GiB>] [--disk <GiB>]")?;
      Cmd::Create {
        system: pos[0].clone(),
        name: value("--name"),
        cpus: number("--cpus")?,
        memory: number("--memory")?,
        disk: number("--disk")?,
      }
    }
    "start" | "stop" | "delete" | "open" | "ssh" | "ip" | "_run" => {
      want(1, "<vm>")?;
      let vm = pos[0].clone();
      match cmd.as_str() {
        "start" => Cmd::Start { vm },
        "stop" => Cmd::Stop { vm, force: flag("--force") },
        "delete" => Cmd::Delete { vm },
        "open" => Cmd::Open { vm },
        "ssh" => Cmd::Ssh { vm },
        "ip" => Cmd::Ip { vm },
        _ => Cmd::Serve { vm, recovery: flag("--recovery") },
      }
    }

    "list" | "ls" => Cmd::List,
    "_reap" => Cmd::Reap,
    "clone" => {
      want(2, "<vm> <name>")?;
      Cmd::Clone { vm: pos[0].clone(), name: pos[1].clone() }
    }
    "os" => return Err(bad("usage: trivm os grant|sip <vm> ...; see trivm help")),
    "os grant" => match pos.len() {
      2.. => Cmd::Grant { vm: pos[0].clone(), app: pos[1].clone(), services: pos[2..].to_vec() },
      _ => return Err(bad("usage: trivm os grant <vm> <app> [<service>...]")),
    },
    "os sip" => {
      want(2, "<vm> on|off")?;
      let on = match pos[1].as_str() {
        "on" => true,
        "off" => false,
        _ => return Err(bad("usage: trivm os sip <vm> on|off")),
      };

      Cmd::Sip { vm: pos[0].clone(), on }
    }
    "cp" => {
      want(2, "<src> <dst>")?;
      Cmd::Cp { src: pos[0].clone(), dst: pos[1].clone() }
    }
    "shot" => match pos.len() {
      1 | 2 => Cmd::Shot { vm: pos[0].clone(), file: pos.get(1).cloned() },
      _ => return Err(bad("usage: trivm shot <vm> [file]")),
    },

    "click" if value("--name").is_some() => {
      want(1, "<vm> --name <text> [--app <name>] [--right] [--double]")?;
      Cmd::ClickName {
        vm: pos[0].clone(),
        name: value("--name").unwrap_or_default(),
        app: value("--app"),
        right: flag("--right"),
        double: flag("--double"),
      }
    }
    "click" => {
      want(3, "<vm> <x> <y> [--right] [--double]")?;
      let (x, y) = (coord(&pos[1])?, coord(&pos[2])?);
      Cmd::Click { vm: pos[0].clone(), x, y, right: flag("--right"), double: flag("--double") }
    }
    "move" => {
      want(3, "<vm> <x> <y>")?;
      Cmd::Move { vm: pos[0].clone(), x: coord(&pos[1])?, y: coord(&pos[2])? }
    }

    "type" => {
      want(2, "<vm> <text>")?;
      Cmd::Type { vm: pos[0].clone(), text: pos[1].clone() }
    }
    "key" => {
      want(2, "<vm> <keys>")?;
      Cmd::Key { vm: pos[0].clone(), keys: pos[1].clone() }
    }
    "mic" => match pos.len() {
      1 | 2 => Cmd::Mic { vm: pos[0].clone(), file: pos.get(1).cloned() },
      _ => return Err(bad("usage: trivm mic <vm> [file]")),
    },
    "ui" => {
      want(1, "<vm> [--app <name>]")?;
      Cmd::Ui { vm: pos[0].clone(), app: value("--app") }
    }
    _ => return Err(bad(format!("unknown command {cmd}; see trivm help"))),
  };

  Ok(cmd)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn args(x: &str) -> io::Result<Cmd> {
    parse(x.split(' ').filter(|x| !x.is_empty()).map(String::from))
  }

  #[test]
  fn commands() {
    assert_eq!(args("").unwrap(), Cmd::Help);
    assert_eq!(args("list").unwrap(), Cmd::List);
    assert_eq!(args("start a --help").unwrap(), Cmd::Help);
    assert_eq!(args("--help").unwrap(), Cmd::Help);
    assert_eq!(args("-h").unwrap(), Cmd::Help);

    assert_eq!(args("stop a --force").unwrap(), Cmd::Stop { vm: "a".into(), force: true });
    assert_eq!(
      args("create macos-27 --name m --disk 80").unwrap(),
      Cmd::Create {
        system: "macos-27".into(),
        name: Some("m".into()),
        cpus: None,
        memory: None,
        disk: Some(80),
      }
    );
    assert!(args("create windows-11 --iso /x/w.iso").is_err());
    assert_eq!(
      args("click a 10 20.5 --double").unwrap(),
      Cmd::Click { vm: "a".into(), x: 10.0, y: 20.5, right: false, double: true }
    );
    assert_eq!(args("shot a").unwrap(), Cmd::Shot { vm: "a".into(), file: None });
    assert_eq!(args("mic a").unwrap(), Cmd::Mic { vm: "a".into(), file: None });
    assert_eq!(
      args("mic a hello.m4a").unwrap(),
      Cmd::Mic { vm: "a".into(), file: Some("hello.m4a".into()) }
    );
    assert!(args("mic a b c").is_err());

    assert_eq!(args("ui a").unwrap(), Cmd::Ui { vm: "a".into(), app: None });
    assert_eq!(
      args("ui a --app Wavo").unwrap(),
      Cmd::Ui { vm: "a".into(), app: Some("Wavo".into()) }
    );
    assert_eq!(
      args("click a --name Models --double").unwrap(),
      Cmd::ClickName {
        vm: "a".into(),
        name: "Models".into(),
        app: None,
        right: false,
        double: true
      }
    );
    assert_eq!(
      args("click a --app Wavo --name General").unwrap(),
      Cmd::ClickName {
        vm: "a".into(),
        name: "General".into(),
        app: Some("Wavo".into()),
        right: false,
        double: false
      }
    );

    assert_eq!(args("cp a:/x .").unwrap(), Cmd::Cp { src: "a:/x".into(), dst: ".".into() });
    assert_eq!(args("clone a b").unwrap(), Cmd::Clone { vm: "a".into(), name: "b".into() });

    assert_eq!(args("os sip a off").unwrap(), Cmd::Sip { vm: "a".into(), on: false });
    assert_eq!(args("os sip a on").unwrap(), Cmd::Sip { vm: "a".into(), on: true });
    assert_eq!(
      args("os grant a /x").unwrap(),
      Cmd::Grant { vm: "a".into(), app: "/x".into(), services: vec![] }
    );
    let services = vec!["screen".into(), "input".into()];
    assert_eq!(
      args("os grant a /x screen input").unwrap(),
      Cmd::Grant { vm: "a".into(), app: "/x".into(), services }
    );
    assert_eq!(args("os --help").unwrap(), Cmd::Help);
    assert_eq!(args("os grant --help").unwrap(), Cmd::Help);
    assert_eq!(args("_run a").unwrap(), Cmd::Serve { vm: "a".into(), recovery: false });
    assert_eq!(args("_run a --recovery").unwrap(), Cmd::Serve { vm: "a".into(), recovery: true });
    assert_eq!(args("type a --x").unwrap(), Cmd::Type { vm: "a".into(), text: "--x".into() });
    assert_eq!(args("type a --help").unwrap(), Cmd::Type { vm: "a".into(), text: "--help".into() });
  }

  #[test]
  fn exec() {
    let cmd = |x: &[&str]| x.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert_eq!(
      args("exec a -- ls -la").unwrap(),
      Cmd::Exec { vm: "a".into(), cmd: cmd(&["ls", "-la"]) }
    );
    assert_eq!(
      args("exec a uname --all").unwrap(),
      Cmd::Exec { vm: "a".into(), cmd: cmd(&["uname", "--all"]) }
    );
    assert!(args("exec a --").is_err());
    assert!(args("exec").is_err());

    assert_eq!(
      args("run a -- ./test.sh --fast").unwrap(),
      Cmd::Run { vm: "a".into(), cmd: cmd(&["./test.sh", "--fast"]) }
    );
    assert_eq!(args("run a make").unwrap(), Cmd::Run { vm: "a".into(), cmd: cmd(&["make"]) });
    assert!(args("run a").is_err());
    assert!(args("run").is_err());
  }

  #[test]
  fn errors() {
    assert!(args("start").is_err());
    assert!(args("start a b").is_err());
    assert!(args("clone a").is_err());
    assert!(args("create x --cpus many").is_err());
    assert!(args("click a -1 2").is_err());

    assert!(args("click a 1 2 --name x").is_err());
    assert!(args("click a --name").is_err());
    assert!(args("ui").is_err());
    assert!(args("ui a --app").is_err());
    assert!(args("start a --what").is_err());
    assert!(args("frobnicate").is_err());
  }

  #[test]
  fn os() {
    assert!(args("os sip a").is_err());
    assert!(args("os sip a no").is_err());
    assert!(args("os grant a").is_err());
    assert_eq!(args("os sip a").unwrap_err().to_string(), "usage: trivm os sip <vm> on|off");
    assert!(args("os").is_err());
    assert!(args("os what a").is_err());

    // No longer commands of their own.
    assert!(args("sip a off").is_err());
    assert!(args("grant a /x").is_err());
  }
}
