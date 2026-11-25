// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! trivm: macOS and Linux VMs on Apple silicon for coding agents to test apps in. A VM runs in a
//! process of its own (`vz.rs`), commands and files go through SSH (`ssh.rs`), the screen is read
//! and driven from the host (`display.rs`), `create` installs a system (`system.rs`), `clone`
//! and `run` copy one (`clone.rs`), and `sip` and `grant` give the app under test its macOS
//! permissions (`permissions.rs`).

mod args;
mod clone;
mod display;
mod permissions;
mod qcow2;
mod ssh;
mod system;
mod vm;
mod vz;

use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};
use std::{env, fs, io, thread};

use args::Cmd;
use vm::{Os, Vm};

fn main() -> ExitCode {
  if env::var_os(ssh::ASKPASS).is_some() {
    println!("{}", vm::PASSWORD);
    return ExitCode::SUCCESS;
  }

  match start_cli() {
    Ok(code) => code,
    Err(e) => {
      eprintln!("trivm: {e}");
      ExitCode::FAILURE
    }
  }
}

fn start_cli() -> io::Result<ExitCode> {
  // A command ended by a signal exits as a shell has it: 128 + the signal.
  let status = |x: std::process::ExitStatus| {
    let code = x.code().or(x.signal().map(|x| 128 + x)).unwrap_or(1);
    ExitCode::from(code.clamp(0, 255) as u8)
  };

  match args::parse(env::args().skip(1))? {
    Cmd::Help => print!("{}", args::HELP),
    Cmd::Version => println!("trivm {}", env!("CARGO_PKG_VERSION")),
    Cmd::Create { system, name, cpus, memory, disk } => {
      signed()?;
      system::create(&system, name, cpus, memory, disk)?;
    }
    Cmd::Start { vm } => start(&vm)?,
    Cmd::Stop { vm, force } => stop(&Vm::open(&vm)?, force)?,
    Cmd::List => list()?,
    Cmd::Delete { vm } => delete(&Vm::open(&vm)?)?,
    Cmd::Clone { vm, name } => _ = clone::clone(&vm, &name, vm::random_mac()?, |_| Ok(()))?,
    Cmd::Run { vm, cmd } => return Ok(status(clone::run(&vm, &cmd)?)),
    Cmd::Sip { vm, on } => permissions::sip(&vm, on)?,
    Cmd::Grant { vm, app, services } => permissions::grant(&vm, &app, &services)?,
    Cmd::Open { vm } => {
      if Vm::open(&vm)?.pid().is_none() {
        start(&vm)?;
      }

      vz::ask(&Vm::open(&vm)?, "open", "")?;
    }
    Cmd::Exec { vm, cmd } => return Ok(status(ssh::exec(&Vm::open(&vm)?, &cmd)?)),
    Cmd::Ssh { vm } => return Ok(status(ssh::interactive(&Vm::open(&vm)?)?)),
    Cmd::Cp { src, dst } => return Ok(status(ssh::cp(&src, &dst)?)),
    Cmd::Ip { vm } => println!("{}", ssh::ip(&Vm::open(&vm)?)?),
    Cmd::Shot { vm, file } => {
      let vm = Vm::open(&vm)?;
      let path = match file {
        Some(x) => std::path::absolute(x)?,
        None => env::temp_dir().join(format!("trivm-{}-{}.png", vm.name, unix_ms())),
      };

      vz::ask(&vm, "shot", &path.to_string_lossy())?;
      println!("{}", path.display());
    }
    Cmd::Click { vm, x, y, right, double } => {
      let (button, clicks) = (if right { "right" } else { "left" }, if double { 2 } else { 1 });
      vz::ask(&Vm::open(&vm)?, &format!("click {x} {y} {button} {clicks}"), "")?;
    }
    Cmd::Move { vm, x, y } => _ = vz::ask(&Vm::open(&vm)?, &format!("move {x} {y}"), "")?,
    Cmd::Type { vm, text } => _ = vz::ask(&Vm::open(&vm)?, "type", &text)?,
    Cmd::Key { vm, keys } => _ = vz::ask(&Vm::open(&vm)?, &format!("key {keys}"), "")?,
    Cmd::Serve { vm, recovery } => {
      signed()?;
      vz::serve(Vm::open(&vm)?, recovery)?;
    }
    Cmd::Reap => clone::reap()?,
  }

  Ok(ExitCode::SUCCESS)
}

pub fn bad(msg: impl Into<String>) -> io::Error {
  io::Error::other(msg.into())
}

/// The output of `cmd`, which must succeed; when it fails, the error has its stderr.
pub fn run(cmd: &mut Command) -> io::Result<Vec<u8>> {
  let name = cmd.get_program().to_string_lossy().into_owned();
  let out = match cmd.output() {
    Ok(x) => x,
    Err(e) if e.kind() == io::ErrorKind::NotFound => {
      return Err(io::Error::new(e.kind(), format!("{name} not found")));
    }
    Err(e) => return Err(bad(format!("{name}: {e}"))),
  };

  if !out.status.success() {
    let err = String::from_utf8_lossy(&out.stderr);
    let err = if err.trim().is_empty() { String::from_utf8_lossy(&out.stdout) } else { err };
    return Err(bad(format!("{name} failed ({}): {}", out.status, err.trim())));
  }

  Ok(out.stdout)
}

fn unix_ms() -> u128 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|x| x.as_millis())
    .unwrap_or(0)
}

const ENTITLEMENTS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>com.apple.security.virtualization</key><true/></dict></plist>
"#;

/// VZ runs only in a binary signed with the virtualization entitlement: trivm signs itself ad hoc
/// the first time (a copy signed, then renamed over the running binary) and runs again. The binary
/// is the file a symlink to it names: `current_exe` is the path trivm was run by.
fn signed() -> io::Result<()> {
  let exe = env::current_exe()?.canonicalize()?;
  let out =
    Command::new("codesign").args(["-d", "--entitlements", "-", "--xml"]).arg(&exe).output()?;
  if String::from_utf8_lossy(&out.stdout).contains("com.apple.security.virtualization") {
    return Ok(());
  }

  if env::var_os("TRIVM_SIGNED").is_some() {
    return Err(bad(format!(
      "{} lacks the virtualization entitlement after signing",
      exe.display()
    )));
  }

  let plist = vm::cache_dir().join("entitlements.plist");
  vm::made(&vm::cache_dir())?;
  fs::write(&plist, ENTITLEMENTS)?;
  let copy = exe.with_extension(format!("signing{}", std::process::id()));
  fs::copy(&exe, &copy)?;
  run(
    Command::new("codesign")
      .args(["--force", "--sign", "-", "--entitlements"])
      .arg(&plist)
      .arg(&copy),
  )?;
  fs::rename(&copy, &exe)?;
  eprintln!("trivm: signed {} with the virtualization entitlement", exe.display());
  Err(Command::new(&exe).args(env::args_os().skip(1)).env("TRIVM_SIGNED", "1").exec())
}

// MARK: Lifecycle

/// Starts the VM's process unless it runs, and waits until the guest answers SSH and its desktop
/// is up; a new VM's first boot sets it up.
pub fn start(name: &str) -> io::Result<()> {
  let mut vm = Vm::open(name)?;
  if vm.pid().is_none() {
    spawn(&vm, false)?;
  }

  match (vm.setup, vm.config.os()) {
    (true, Os::Macos) => system::macos_setup(&vm)?,
    (true, Os::Linux) => system::linux_setup(&vm)?,
    (false, _) => ssh::wait(&vm, Duration::from_secs(300))?,
  }

  system::desktop(&vm)?;
  // Set up only once the desktop is up: a setup that didn't bring it up runs again next time.
  if vm.setup {
    vm.setup = false;
    vm.save()?;
  }

  eprintln!("{name} is up at {}", ssh::ip(&vm)?);
  Ok(())
}

/// Runs the VM's process in a session of its own, macOS's from recoveryOS with `recovery`, and
/// waits until the VM runs, in it or in another `start`'s, whose lock makes this one exit.
pub fn spawn(vm: &Vm, recovery: bool) -> io::Result<()> {
  let name = &vm.name;
  vm::made(&vm.state_dir)?;
  let log = fs::OpenOptions::new().create(true).append(true).open(vm.state("log"))?;
  let mut cmd = Command::new(env::current_exe()?);
  cmd
    .args(["_run", name])
    .args(recovery.then_some("--recovery"))
    .env_remove("TRIVM_SIGNED")
    .stdin(Stdio::null())
    .stdout(log.try_clone()?)
    .stderr(log);
  detach(&mut cmd);
  let mut child = cmd.spawn()?;

  let t = Instant::now();
  loop {
    if let Some(code) = child.try_wait()?
      && vm.pid().is_none()
    {
      let log = fs::read_to_string(vm.state("log")).unwrap_or_default();
      let tail: Vec<_> = log.lines().rev().take(5).collect();
      let tail: Vec<_> = tail.into_iter().rev().collect();
      return Err(bad(format!("{name} didn't start ({code}):\n{}", tail.join("\n"))));
    }

    if vz::ask(vm, "status", "").is_ok_and(|x| x == "running") {
      return Ok(());
    }

    if t.elapsed() > Duration::from_secs(120) {
      return Err(bad(format!(
        "{name} didn't start in 2 minutes; see {}",
        vm.state("log").display()
      )));
    }

    thread::sleep(Duration::from_millis(300));
  }
}

/// Makes `cmd` run in a session of its own, out of the terminal's reach: Ctrl+C or the terminal
/// closed don't end it.
pub fn detach(cmd: &mut Command) {
  // SAFETY: setsid is async-signal-safe.
  unsafe { cmd.pre_exec(|| if setsid() < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }) };
}

unsafe extern "C" {
  fn setsid() -> i32;
}

/// Shuts the guest down from inside, or with `force` stops it at once; waits for the process.
pub fn stop(vm: &Vm, force: bool) -> io::Result<()> {
  let Some(pid) = vm.pid() else { return Ok(()) };
  // Gone once its process no longer holds the lock: the VM started again meanwhile isn't this one.
  let gone = |secs: u64| {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(secs) {
      if vm.pid() != Some(pid) {
        return true;
      }

      thread::sleep(Duration::from_millis(300));
    }

    false
  };

  if !force && vm.ip().is_some() {
    let cmd = match vm.config.os() {
      Os::Macos => "sudo -n shutdown -h now",
      Os::Linux => "sudo -n systemctl poweroff",
    };

    // Not waited for: a guest that hangs in it is still stopped after 2 minutes.
    let mut ssh = ssh::ssh(vm)?;
    ssh.args(["--", cmd]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let child = ssh.spawn();
    let down = gone(120);
    if let Ok(mut x) = child {
      let _ = x.kill();
      let _ = x.wait();
    }

    if down {
      return Ok(());
    }
  }

  // A VM process busy with a long request answers late; SIGKILL follows anyway.
  let _ = vz::ask_within(vm, "kill", "", Duration::from_secs(5));
  if !gone(15) {
    vm::signal(pid, 9);
    if !gone(5) {
      return Err(bad(format!("{} didn't stop; its process is {pid}", vm.name)));
    }
  }

  Ok(())
}

/// Stops the VM at once and removes it, holding its lock: it can't start or be cloned meanwhile,
/// and one being made or cloned isn't removed under it.
fn delete(vm: &Vm) -> io::Result<()> {
  stop(vm, true)?;
  let _lock = vm.take()?;
  vm.remove()
}

fn list() -> io::Result<()> {
  let vms = Vm::all()?;
  if vms.is_empty() {
    eprintln!("no VMs; trivm create <system> makes one");
    return Ok(());
  }

  println!(
    "{:<16} {:<14} {:<9} {:<15} {:>4} {:>6} {:>8}",
    "NAME", "SYSTEM", "STATE", "IP", "CPUS", "MEMORY", "DISK"
  );
  for vm in vms {
    let c = &vm.config;
    let state = match (vm.pid().is_some(), vm.setup) {
      (true, _) => "running",
      (false, true) => "new",
      (false, false) => "stopped",
    };

    let ip = vm.pid().and(vm.ip()).unwrap_or_default();
    let (memory, disk) = (format!("{}G", c.memory), vm::size(&vm.path("disk.img")));
    println!(
      "{:<16} {:<14} {state:<9} {ip:<15} {:>4} {memory:>6} {disk:>8}",
      vm.name, c.system, c.cpus
    );
  }

  Ok(())
}
