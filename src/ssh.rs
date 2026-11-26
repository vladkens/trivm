// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! Commands and files through the system's `ssh` and `scp` with trivm's own key; the password
//! for a guest's first login comes from trivm itself as `SSH_ASKPASS`.

use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{self, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use std::{env, fs, io, thread};

use crate::vm::{self, Os, Vm};
use crate::{bad, run, windows};

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

/// The guest's address and SSH's forwarded port, if any, of a running VM: a stopped VM's may be
/// another VM's by now. The port is read once the VM runs, as a start saves it before that.
pub fn ip(vm: &Vm) -> io::Result<(String, Option<u16>)> {
  if vm.pid().is_none() {
    return Err(bad(format!("{} isn't running; trivm start {}", vm.name, vm.name)));
  }

  let port = vm.saved_port();
  let ip = match port {
    Some(_) => Some("127.0.0.1".to_string()),
    None => vm.ip(),
  };

  Ok((ip.ok_or_else(|| bad(format!("{} has no address yet", vm.name)))?, port))
}

/// Whether the guest's SSH port takes a connection now.
pub fn answers(vm: &Vm) -> bool {
  let addr = ip(vm).ok().and_then(|(ip, port)| format!("{ip}:{}", port.unwrap_or(22)).parse().ok());
  addr.is_some_and(|x: SocketAddr| TcpStream::connect_timeout(&x, Duration::from_secs(2)).is_ok())
}

/// The guest's SSH as `ip` or `ip:port`.
pub fn address(vm: &Vm) -> io::Result<String> {
  Ok(match ip(vm)? {
    (ip, Some(port)) => format!("{ip}:{port}"),
    (ip, None) => ip,
  })
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
  cmd.args(options(&key()?)).args(["-o", "BatchMode=yes"]);
  let (ip, port) = ip(vm)?;
  if let Some(port) = port {
    cmd.args(["-p", &port.to_string()]);
  }

  cmd.arg(format!("{}@{ip}", vm::USER));
  Ok(cmd)
}

/// A script as the guest runs it: Windows' as PowerShell, others' by the user's shell.
fn remote(vm: &Vm, script: &str) -> io::Result<String> {
  match vm.config.os() {
    Os::Windows => windows::powershell(script),
    _ => Ok(script.to_string()),
  }
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
  let line = match vm.config.os() {
    Os::Windows => windows::line(cmd),
    _ => line(cmd),
  };

  ssh(vm)?.arg("--").arg(remote(vm, &line)?).status()
}

/// A shell script's output, which must succeed.
pub fn output(vm: &Vm, script: &str) -> io::Result<String> {
  let out = run(ssh(vm)?.arg("--").arg(remote(vm, script)?).stdin(Stdio::null()))?;
  Ok(String::from_utf8_lossy(&out).trim().to_string())
}

pub fn interactive(vm: &Vm) -> io::Result<ExitStatus> {
  let mut cmd = ssh(vm)?;
  cmd.arg("-t");
  if vm.config.os() == Os::Windows {
    cmd.arg("--").arg(windows::shell());
  }

  cmd.status()
}

/// `scp -r` between the host and VMs, `<vm>:<path>` naming a VM's file; a VM whose SSH is a
/// forwarded port is an `scp://` URL, whose path is from the user's home, or absolute after `/`.
pub fn cp(src: &str, dst: &str) -> io::Result<ExitStatus> {
  let side = |x: &str| -> io::Result<String> {
    let Some((name, path)) = x.split_once(':') else { return Ok(x.to_string()) };
    if name.is_empty() || name.contains('/') {
      return Ok(x.to_string());
    }

    let vm = Vm::open(name)?;
    let (ip, port) = ip(&vm)?;
    Ok(url(&ip, port, path))
  };

  let (src, dst) = (side(src)?, side(dst)?);
  Command::new("scp")
    .args(options(&key()?))
    .args(["-o", "BatchMode=yes", "-r", "-q", &src, &dst])
    .status()
}

/// The guest's side of `scp`: an `scp://` URL with a port, whose path scp decodes (`%` escapes,
/// `+` as a space), so all but plain characters are escaped. Windows' sshd finds no path with `\`
/// in it: `/` in its place.
fn url(ip: &str, port: Option<u16>, path: &str) -> String {
  let Some(port) = port else { return format!("{}@{ip}:{path}", vm::USER) };
  let path: String = path
    .bytes()
    .map(|x| match x {
      b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
        (x as char).to_string()
      }
      b'\\' => "/".to_string(),
      _ => format!("%{x:02X}"),
    })
    .collect();
  format!("scp://{}@{ip}:{port}/{path}", vm::USER)
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
      // `exit 0` runs in cmd, Windows' shell, too.
      match ssh(vm)?.args(["--", "exit 0"]).stdin(Stdio::null()).output() {
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

  #[test]
  fn urls() {
    assert_eq!(url("192.168.64.2", None, "/tmp/"), "user@192.168.64.2:/tmp/");
    assert_eq!(url("127.0.0.1", Some(50022), "C:/x"), "scp://user@127.0.0.1:50022/C:/x");
    assert_eq!(url("127.0.0.1", Some(50022), ""), "scp://user@127.0.0.1:50022/");
    let path = "C:\\a+b 100%/é@x";
    assert_eq!(
      url("127.0.0.1", Some(1), path),
      "scp://user@127.0.0.1:1/C:/a%2Bb%20100%25/%C3%A9%40x"
    );
    assert_eq!(url("10.0.0.2", None, "/a+b 100%"), "user@10.0.0.2:/a+b 100%");
  }
}
