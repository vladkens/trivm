// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! Commands and files through the system's `ssh` and `scp` with trivm's own key; the password
//! for a guest's first login comes from trivm itself as `SSH_ASKPASS`.

use std::path::{Path, PathBuf};
use std::process::{self, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use std::{env, fs, io, thread};

use crate::vm::{self, Vm};
use crate::{bad, run};

/// Set when trivm runs as `ssh`'s askpass program: it prints the guest's password.
pub const ASKPASS: &str = "TRIVM_ASKPASS";

/// trivm's key, made on first use beside its place, then moved there, the public half last: a
/// public half there is whole.
pub fn key() -> io::Result<PathBuf> {
  let path = vm::data_dir().join("id_ed25519");
  if path.with_extension("pub").exists() {
    return Ok(path);
  }

  // Two first uses at once make one key.
  vm::made(&vm::data_dir())?;
  let lock = fs::File::create(path.with_extension("lock"))?;
  lock.lock()?;
  if !path.with_extension("pub").exists() {
    let new = path.with_extension(format!("new{}", process::id()));
    let public = |x: &Path| PathBuf::from(format!("{}.pub", x.display()));
    let _ = fs::remove_file(&new);
    let _ = fs::remove_file(public(&new));
    run(
      Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "trivm", "-f"])
        .arg(&new),
    )?;
    fs::rename(&new, &path)?;
    fs::rename(public(&new), public(&path))?;
  }

  Ok(path)
}

pub fn public_key() -> io::Result<String> {
  Ok(fs::read_to_string(key()?.with_extension("pub"))?.trim().to_string())
}

pub fn ip(vm: &Vm) -> io::Result<String> {
  vm.ip().ok_or_else(|| bad(format!("{} has no address yet", vm.name)))
}

/// Options for both `ssh` and `scp`: trivm's key, the guest's host key neither checked nor kept
/// (a new VM has a new one at the same address).
fn options(key: &Path) -> Vec<String> {
  let key = key.to_string_lossy();
  let opts = [
    "StrictHostKeyChecking=no",
    "UserKnownHostsFile=/dev/null",
    "LogLevel=ERROR",
    "ConnectTimeout=5",
    "ServerAliveInterval=10",
    "IdentitiesOnly=yes",
  ];
  let mut args = vec!["-i".to_string(), key.into_owned()];
  args.extend(opts.iter().flat_map(|x| ["-o".to_string(), x.to_string()]));
  args
}

/// `ssh` to the guest, its command line to add.
pub fn ssh(vm: &Vm) -> io::Result<Command> {
  let mut cmd = Command::new("ssh");
  cmd.args(options(&key()?)).args(["-o", "BatchMode=yes"]).arg(format!("{}@{}", vm::USER, ip(vm)?));
  Ok(cmd)
}

/// The remote command line: one argument is a shell line as it is, more are quoted each.
pub fn line(cmd: &[String]) -> String {
  match cmd {
    [one] => one.clone(),
    _ => cmd.iter().map(|x| quote(x)).collect::<Vec<_>>().join(" "),
  }
}

pub fn quote(x: &str) -> String {
  let safe =
    !x.is_empty() && x.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c));
  match safe {
    true => x.to_string(),
    false => format!("'{}'", x.replace('\'', r"'\''")),
  }
}

pub fn exec(vm: &Vm, cmd: &[String]) -> io::Result<ExitStatus> {
  ssh(vm)?.arg("--").arg(line(cmd)).status()
}

/// A shell script's output, which must succeed.
pub fn output(vm: &Vm, script: &str) -> io::Result<String> {
  let out = run(ssh(vm)?.arg("--").arg(script).stdin(Stdio::null()))?;
  Ok(String::from_utf8_lossy(&out).trim().to_string())
}

pub fn interactive(vm: &Vm) -> io::Result<ExitStatus> {
  ssh(vm)?.arg("-t").status()
}

/// `scp -r` between the host and VMs, `<vm>:<path>` naming a VM's file.
pub fn cp(src: &str, dst: &str) -> io::Result<ExitStatus> {
  let side = |x: &str| -> io::Result<String> {
    let Some((name, path)) = x.split_once(':') else { return Ok(x.to_string()) };
    if name.is_empty() || name.contains('/') {
      return Ok(x.to_string());
    }

    let vm = Vm::open(name)?;
    Ok(format!("{}@{}:{path}", vm::USER, ip(&vm)?))
  };

  let (src, dst) = (side(src)?, side(dst)?);
  Command::new("scp")
    .args(options(&key()?))
    .args(["-o", "BatchMode=yes", "-r", "-q", &src, &dst])
    .status()
}

/// Waits until the guest runs a command over SSH with trivm's key.
pub fn wait(vm: &Vm, timeout: Duration) -> io::Result<()> {
  let start = Instant::now();
  let mut last = "no address yet".to_string();
  while start.elapsed() < timeout {
    if vm.pid().is_none() {
      return Err(bad(format!("{} stopped; see {}", vm.name, vm.state("log").display())));
    }

    if vm.ip().is_some() {
      match ssh(vm)?.args(["--", "true"]).stdin(Stdio::null()).output() {
        Ok(x) if x.status.success() => return Ok(()),
        Ok(x) => last = String::from_utf8_lossy(&x.stderr).trim().to_string(),
        Err(e) => last = e.to_string(),
      }
    }

    thread::sleep(Duration::from_secs(3));
  }

  Err(bad(format!("{} doesn't answer SSH: {last}", vm.name)))
}

/// Runs `script` as the guest's user with the password, trivm answering ssh's prompt: a macOS
/// guest's first login, before it has trivm's key, or with the key once it has (a setup cut
/// short). `sudo -S` in it reads the password from stdin.
pub fn with_password(vm: &Vm, script: &str, timeout: Duration) -> io::Result<()> {
  let exe = env::current_exe()?;
  let start = Instant::now();
  let mut last = "no address yet".to_string();
  while start.elapsed() < timeout {
    if vm.pid().is_none() {
      return Err(bad(format!("{} stopped; see {}", vm.name, vm.state("log").display())));
    }

    if let Some(ip) = vm.ip() {
      let mut cmd = Command::new("ssh");
      cmd.args(options(&key()?));
      cmd.args(["-o", "BatchMode=no", "-o", "NumberOfPasswordPrompts=1"]);
      cmd.args(["-o", "PreferredAuthentications=publickey,password,keyboard-interactive"]);
      cmd.arg(format!("{}@{ip}", vm::USER)).arg("--").arg(script);
      cmd.env("SSH_ASKPASS", &exe).env("SSH_ASKPASS_REQUIRE", "force").env(ASKPASS, "1");
      cmd
        .env("DISPLAY", "trivm")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

      let mut child = cmd.spawn()?;
      if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = writeln!(stdin, "{}", vm::PASSWORD);
      }

      let out = child.wait_with_output()?;
      if out.status.success() {
        return Ok(());
      }

      last = String::from_utf8_lossy(&out.stderr).trim().to_string();
    }

    thread::sleep(Duration::from_secs(5));
  }

  Err(bad(format!("{} doesn't take the password over SSH: {last}", vm.name)))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn lines() {
    let v = |x: &[&str]| x.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert_eq!(line(&v(&["ls -la | wc -l"])), "ls -la | wc -l");
    assert_eq!(line(&v(&["echo", "a b", "it's", "x=1"])), r"echo 'a b' 'it'\''s' x=1");
    assert_eq!(quote(""), "''");
  }
}
