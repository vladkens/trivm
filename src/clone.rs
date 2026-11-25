// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! Clones of a stopped VM, copied with APFS's `clonefile`, which shares the blocks until either
//! copy writes them: `clone` makes one to keep, `run` one for a command on the host, deleted after
//! however the command ends.

use std::ffi::{CString, c_char};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;
use std::{env, fs, thread};

use crate::vm::{self, Config, Vm};
use crate::{bad, vz};

/// A clone of a stopped VM that is set up, with the MAC address given and a new machine
/// identifier. Its name is claimed by its data folder, which `made` is told of first, and its
/// config written last: no one opens half a VM, and of two clones of a name one fails.
pub fn clone(
  src: &str,
  name: &str,
  mac: String,
  made: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<Vm> {
  let vm = Vm::open(src)?;
  if vm.setup {
    return Err(bad(format!("{src} isn't set up yet; trivm start {src} finishes it")));
  }

  // Held while copying, so the VM can't start in the middle; the clone held until it shows, and
  // while a failed one goes.
  let _hold = vm.hold()?;
  let new = Vm { mac, ..Vm::at(name, vm.config.clone()) };
  let _made = new.claim()?;
  let res = made(&new.dir).and_then(|_| copy(&vm.dir, &new)).and_then(|_| new.commit());
  if let Err(e) = res {
    let _ = new.remove();
    return Err(e);
  }

  Vm::open(name)
}

/// The VM's data into `vm`'s folder but its `mac`, which the clone has its own of, then a new
/// machine identifier.
fn copy(src: &Path, vm: &Vm) -> io::Result<()> {
  for x in fs::read_dir(src)? {
    let x = x?;
    let (name, kind) = (x.file_name(), x.file_type()?);
    if name == "mac" || !(kind.is_file() || kind.is_dir()) {
      continue;
    }

    clone_file(&x.path(), &vm.dir.join(&name))?;
  }

  fs::write(vm.path("machine.bin"), vz::machine_id(vm.config.os()))
}

unsafe extern "C" {
  fn clonefile(src: *const c_char, dst: *const c_char, flags: u32) -> i32;
}

/// An APFS clone of a file, or of a folder with all in it.
fn clone_file(src: &Path, dst: &Path) -> io::Result<()> {
  let c = |x: &Path| CString::new(x.as_os_str().as_bytes()).map_err(|_| bad("a path with NUL"));
  let (a, b) = (c(src)?, c(dst)?);
  // SAFETY: two NUL-terminated paths that live through the call.
  if unsafe { clonefile(a.as_ptr(), b.as_ptr(), 0) } != 0 {
    let e = io::Error::last_os_error();
    return Err(bad(format!("cloning {}: {e}; clones need APFS", src.display())));
  }

  Ok(())
}

// MARK: run

/// `trivm run <vm> -- <cmd>`: the command on the host with a new clone of the VM, started, named
/// in `TRIVM_VM`; the clone deleted after. The command's exit status.
pub fn run(src: &str, cmd: &[String]) -> io::Result<ExitStatus> {
  let mut reaper = reaper()?;
  let vm = slot(src, &mut reaper)?;
  let ino = fs::metadata(&vm.dir)?.ino();
  let status = crate::start(&vm.name).and_then(|_| host(&vm.name, cmd));
  match discard(&vm, ino) {
    Ok(()) => _ = writeln!(reaper, "done"),
    // The reaper tries again once this process ends.
    Err(e) => eprintln!("trivm: deleting {}: {e}", vm.name),
  }

  status
}

/// `run`'s clone of `src`: `<src>-run-<n>`, the lowest `n` free, with a MAC address of its own
/// for each `n`. The reaper is told of each try before its files are copied.
fn slot(src: &str, reaper: &mut io::PipeWriter) -> io::Result<Vm> {
  let mac = Vm::open(src)?.mac;
  let mut n = 1;
  loop {
    let name = format!("{src}-run-{n}");
    let tell = |dir: &Path| writeln!(reaper, "{} {name}", fs::metadata(dir)?.ino());

    match clone(src, &name, vm::clone_mac(&mac, n)?, tell) {
      Err(e) if e.kind() == io::ErrorKind::AlreadyExists && n < u8::MAX => n += 1,
      x => return x,
    }
  }
}

unsafe extern "C" {
  fn signal(sig: i32, handler: usize) -> usize;
}

/// The command on the host, one argument a shell line, with the clone's name in `TRIVM_VM`.
/// Ctrl+C reaches it from the terminal; trivm, which ignores it, deletes the clone once it ends.
fn host(name: &str, cmd: &[String]) -> io::Result<ExitStatus> {
  let child = match cmd {
    [line] => Command::new("sh").arg("-c").arg(line).env("TRIVM_VM", name).spawn(),
    [exe, args @ ..] => Command::new(exe).args(args).env("TRIVM_VM", name).spawn(),
    [] => return Err(bad("run: no command given")),
  };

  let mut child = child.map_err(|e| bad(format!("{}: {e}", cmd[0])))?;

  // SAFETY: SIGINT (2) ignored (SIG_IGN, 1), after the command started with the default.
  unsafe { signal(2, 1) };
  child.wait()
}

/// The reaper: `trivm _reap` in a session of its own, which outlives `run`. `run` holds the only
/// other end of its stdin, so the input ends when `run` does, however it does.
fn reaper() -> io::Result<io::PipeWriter> {
  let (rx, tx) = io::pipe()?;
  let mut cmd = Command::new(env::current_exe()?);
  cmd.arg("_reap").stdin(rx).stdout(Stdio::null()).stderr(Stdio::null());
  crate::detach(&mut cmd);
  cmd.spawn()?;
  Ok(tx)
}

/// `trivm _reap`: deletes the clone of a `run` that ended without deleting it. `run` writes, for
/// each name it tries, the inode of the clone's data folder (APFS doesn't reuse inodes) and the
/// name, then `done` once it deleted it.
pub fn reap() -> io::Result<()> {
  let text = io::read_to_string(io::stdin())?;
  let Some(last) = text.lines().last() else { return Ok(()) };
  let Some((ino, name)) = last.split_once(' ') else { return Ok(()) };
  let ino: u64 = ino.parse().map_err(|_| bad(format!("_reap: not an inode: {ino}")))?;
  // Its folders are enough: the config may not be written yet.
  discard(&Vm::at(name, Config::default()), ino)
}

/// Stops and deletes `run`'s clone if its data folder still has the inode it was made with,
/// checked again under its lock: the folder may have been deleted meanwhile and the name taken by
/// another.
fn discard(vm: &Vm, ino: u64) -> io::Result<()> {
  let ours = || fs::metadata(&vm.dir).is_ok_and(|x| x.ino() == ino);
  if !ours() {
    return Ok(());
  }

  crate::stop(vm, true)?;
  // A clone of it being made holds it a moment. Taken with no pid, as `delete` takes a VM.
  let mut lock = vm.take();
  for _ in 0..60 {
    if lock.is_ok() {
      break;
    }

    thread::sleep(Duration::from_secs(1));
    lock = vm.take();
  }

  let _lock = lock?;
  if !ours() {
    return Ok(());
  }

  // Its data folder, by which it is known, last: a removal cut short, the reaper finishes it.
  vm::gone(fs::remove_file(&vm.conf))?;
  vm::gone(fs::remove_dir_all(&vm.state_dir))?;
  fs::remove_dir_all(&vm.dir)
}

#[cfg(test)]
mod tests {
  use std::process;

  use super::*;

  #[test]
  fn discards() {
    let root = env::temp_dir().join(format!("trivm-discard-{}", process::id()));
    let vm = Vm {
      conf: root.join("a.conf"),
      dir: root.join("data/a"),
      state_dir: root.join("state/a"),
      ..Vm::at("a", Config::default())
    };
    let make = || {
      fs::create_dir_all(&vm.dir).unwrap();
      fs::create_dir_all(&vm.state_dir).unwrap();
      fs::write(&vm.conf, "").unwrap();
      fs::metadata(&vm.dir).unwrap().ino()
    };

    // Another VM's folder by that name: left alone.
    let ino = make();
    discard(&vm, ino + 1).unwrap();
    assert!(vm.dir.exists() && vm.state_dir.exists() && vm.conf.exists());

    // Held, as by a clone of it being made: waited for.
    let hold = vm.hold().unwrap();
    let t = std::time::Instant::now();
    thread::scope(|s| {
      s.spawn(|| {
        thread::sleep(Duration::from_millis(500));
        drop(hold);
      });
      discard(&vm, ino).unwrap();
    });
    assert!(t.elapsed() >= Duration::from_millis(500));
    assert!(!vm.dir.exists() && !vm.state_dir.exists() && !vm.conf.exists());

    // One whose removal was cut short, its config and state gone: its data goes too.
    fs::create_dir_all(&vm.dir).unwrap();
    discard(&vm, fs::metadata(&vm.dir).unwrap().ino()).unwrap();
    assert!(!vm.dir.exists() && !vm.state_dir.exists());
    fs::remove_dir_all(&root).unwrap();
  }
}
