// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! The command line: trivm's commands and their arguments, parsed by hand, and the help.

use std::io;

use crate::bad;

pub const HELP: &str = "\
trivm: macOS, Linux and Windows VMs on Apple silicon for coding agents to test apps in.

Usage: trivm <command> [args]

VMs:
  create <system> [--name <vm>] [--cpus <n>] [--memory <GiB>] [--disk <GiB>] [--iso <path>]
                          install a system unattended and start it; systems: macos-27,
                          ubuntu-24.04, windows-11 (its ISO from Microsoft's page by --iso, once)
  start <vm>              start in the background, wait for the desktop
  stop <vm> [--force]     shut down; --force stops at once
  list                    the VMs, their systems and states
  delete <vm>             stop and remove
  open <vm>               show the VM's window, to look or use the mouse

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

The guest's user is `user`, password `1234`, sudo without one (an administrator on Windows).
Files live in ~/.trivm (TRIVM_HOME).
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
    iso: Option<String>,
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
  /// The process that runs a VM, started by `start`.
  Run {
    vm: String,
  },
}

pub fn parse(args: impl IntoIterator<Item = String>) -> io::Result<Cmd> {
  let mut args = args.into_iter();
  let Some(cmd) = args.next() else { return Ok(Cmd::Help) };
  let rest: Vec<String> = args.collect();

  if cmd == "exec" {
    let mut rest = rest.into_iter();
    let vm = rest.next().ok_or_else(|| bad("exec: no VM given"))?;
    let mut cmd: Vec<String> = rest.collect();
    if cmd.first().is_some_and(|x| x == "--") {
      cmd.remove(0);
    }

    if cmd.is_empty() {
      return Err(bad("exec: no command given"));
    }

    return Ok(Cmd::Exec { vm, cmd });
  }

  let mut pos = Vec::new();
  let mut flags = Vec::new();
  let mut it = rest.into_iter();
  while let Some(x) = it.next() {
    match x.as_str() {
      // `type` takes any text, dashes included.
      _ if cmd == "type" => pos.push(x),
      "--name" | "--cpus" | "--memory" | "--disk" | "--iso" => {
        let value = it.next().ok_or_else(|| bad(format!("{x}: no value given")))?;
        flags.push((x, Some(value)));
      }
      "--force" | "--right" | "--double" | "-h" | "--help" => flags.push((x, None)),
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
      want(
        1,
        "<system> [--name <vm>] [--cpus <n>] [--memory <GiB>] [--disk <GiB>] [--iso <path>]",
      )?;
      Cmd::Create {
        system: pos[0].clone(),
        name: value("--name"),
        cpus: number("--cpus")?,
        memory: number("--memory")?,
        disk: number("--disk")?,
        iso: value("--iso"),
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
        _ => Cmd::Run { vm },
      }
    }

    "list" | "ls" => Cmd::List,
    "cp" => {
      want(2, "<src> <dst>")?;
      Cmd::Cp { src: pos[0].clone(), dst: pos[1].clone() }
    }
    "shot" => match pos.len() {
      1 | 2 => Cmd::Shot { vm: pos[0].clone(), file: pos.get(1).cloned() },
      _ => return Err(bad("usage: trivm shot <vm> [file]")),
    },

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
        iso: None
      }
    );
    assert_eq!(
      args("create windows-11 --iso /x/w.iso").unwrap(),
      Cmd::Create {
        system: "windows-11".into(),
        name: None,
        cpus: None,
        memory: None,
        disk: None,
        iso: Some("/x/w.iso".into())
      }
    );
    assert_eq!(
      args("click a 10 20.5 --double").unwrap(),
      Cmd::Click { vm: "a".into(), x: 10.0, y: 20.5, right: false, double: true }
    );
    assert_eq!(args("shot a").unwrap(), Cmd::Shot { vm: "a".into(), file: None });
    assert_eq!(args("cp a:/x .").unwrap(), Cmd::Cp { src: "a:/x".into(), dst: ".".into() });
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
  }

  #[test]
  fn errors() {
    assert!(args("start").is_err());
    assert!(args("start a b").is_err());
    assert!(args("create x --cpus many").is_err());
    assert!(args("click a -1 2").is_err());
    assert!(args("start a --what").is_err());
    assert!(args("frobnicate").is_err());
  }
}
