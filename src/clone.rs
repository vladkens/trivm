// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! Clones of a stopped VM, copied with APFS's `clonefile`, which shares the blocks until either
//! copy writes them: `clone` makes one to keep, `run` one for a command on the host, deleted after
//! however the command ends.

use std::ffi::{CString, c_char};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{self, Command, ExitStatus, Stdio};
use std::time::Duration;
use std::{env, fs, thread};

use crate::vm::{self, Config, Vm};
use crate::{bad, vz};

/// The files of a VM's folder that are its runs', not the machine's: not cloned, nor the config
/// (`config` and the `config.<pid>` it is saved through), which the clone has its own of.
const OWN: &[&str] = &["pid", "log", "console.log", "ctl.sock"];

/// A clone of a stopped VM that is set up, with the MAC address given and a new machine
/// identifier. It is made whole in a hidden folder of this process's, which `made` is told of
/// first, then renamed into place: no one sees half a VM, and of two clones of a name one fails.
pub fn clone(
  src: &str,
  name: &str,
  mac: String,
  made: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<Vm> {
  let vm = Vm::open(src)?;
  if vm.config.setup {
    return Err(bad(format!("{src} isn't set up yet; trivm start {src} finishes it")));
  }

  if !vm::valid(name) {
    return Err(bad(format!("not a VM name: {name}")));
  }

  let dst = vm::home().join(name);
  vm::socket_fits(&dst)?;
  let taken = || {
    let msg = format!("{name} exists; trivm delete {name} first");
    io::Error::new(io::ErrorKind::AlreadyExists, msg)
  };
  if dst.exists() {
    return Err(taken());
  }

  // Held while copying, so the VM can't start in the middle.
  let _hold = vm.hold()?;
  let tmp = vm::home().join(format!(".{name}.{}", process::id()));
  let _ = fs::remove_dir_all(&tmp);
  fs::create_dir(&tmp)?;
  vm::private(&tmp)?;

  let new = Vm { name: name.into(), dir: tmp.clone(), config: Config { mac, ..vm.config } };
  let res = made(&tmp).and_then(|_| copy(&vm.dir, &new)).and_then(|_| {
    // rename(2) doesn't replace a folder with files in it.
    match fs::rename(&tmp, &dst) {
      Err(_) if dst.exists() => Err(taken()),
      x => x,
    }
  });
  if let Err(e) = res {
    let _ = fs::remove_dir_all(&tmp);
    return Err(e);
  }

  Vm::open(name)
}

/// The VM's files into `vm`'s folder but the run's own, a new machine identifier, the config.
fn copy(src: &Path, vm: &Vm) -> io::Result<()> {
  for x in fs::read_dir(src)? {
    let x = x?;
    let (name, kind) = (x.file_name(), x.file_type()?);
    let config = name.to_str().is_some_and(|x| x == "config" || x.starts_with("config."));
    if config || OWN.iter().any(|x| name == *x) || !(kind.is_file() || kind.is_dir()) {
      continue;
    }

    clone_file(&x.path(), &vm.dir.join(&name))?;
  }

  fs::write(vm.path("machine.bin"), vz::machine_id(vm.config.os()))?;
  vm.save()
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
  let mac = Vm::open(src)?.config.mac;
  let mut n = 1;
  loop {
    let name = format!("{src}-run-{n}");
    let tell =
      |tmp: &Path| writeln!(reaper, "{} {name} {}", fs::metadata(tmp)?.ino(), tmp.display());

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
/// each name it tries, the inode of the hidden folder the clone is made in (kept when it's renamed
/// into place; APFS doesn't reuse inodes), the name and the folder, then `done` once it deleted it.
pub fn reap() -> io::Result<()> {
  let text = io::read_to_string(io::stdin())?;
  let Some(last) = text.lines().last() else { return Ok(()) };
  let mut words = last.splitn(3, ' ');
  let (Some(ino), Some(name), Some(tmp)) = (words.next(), words.next(), words.next()) else {
    return Ok(());
  };

  let ino: u64 = ino.parse().map_err(|_| bad(format!("_reap: not an inode: {ino}")))?;
  if fs::metadata(tmp).is_ok_and(|x| x.ino() == ino) {
    return fs::remove_dir_all(tmp);
  }

  discard(&Vm::open(name)?, ino)
}

/// Stops and deletes `run`'s clone if its folder still has the inode it was made with, checked
/// again under its lock: the folder may have been deleted meanwhile and the name taken by another.
fn discard(vm: &Vm, ino: u64) -> io::Result<()> {
  let ours = || fs::metadata(&vm.dir).is_ok_and(|x| x.ino() == ino);
  if !ours() {
    return Ok(());
  }

  crate::stop(vm, true)?;
  // A clone of it being made holds the lock a moment.
  let mut lock = vm.lock();
  for _ in 0..60 {
    if lock.is_ok() {
      break;
    }

    thread::sleep(Duration::from_secs(1));
    lock = vm.lock();
  }

  let _lock = lock?;
  match ours() {
    true => fs::remove_dir_all(&vm.dir),
    false => Ok(()),
  }
}
