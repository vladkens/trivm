// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! Commands and files through the system's `ssh` and `scp` with trivm's own key, `exec`'s with
//! `desktop` on macOS and Linux; the password for a guest's first login comes from trivm itself
//! as `SSH_ASKPASS`.

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
    !x.is_empty() && x.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:@%+,".contains(c));
  match safe {
    true => x.to_string(),
    false => format!("'{}'", x.replace('\'', r"'\''")),
  }
}

/// `desktop <program> [args]` on macOS: a launchd job in the user's GUI session, as SSH has no API
/// to start one there; launchd's, the program has its own TCC grants, not sshd's. Booting a job out
/// kills its program: finished jobs go at a later call, once their folder, their call's, is gone.
const MACOS_DESKTOP: &str = r#"desktop() {
  if [ $# -eq 0 ]; then
    echo 'usage: desktop <program> [args]' >&2
    return 2
  fi
  local exe=$1 uid tmp d job s=0 x n=0 up=0
  case $exe in
    */*) ;;
    *) exe=$(whence -p -- "$exe") || exe= ;;
  esac
  case $exe in
    /*) ;;
    ?*) exe=$PWD/$exe ;;
  esac
  exe=${exe%/}
  if [ -d "$exe" ] && [ "${exe%.app}" != "$exe" ]; then
    shift
    if [ $# -gt 0 ]; then open -a "$exe" --args "$@"; else open -a "$exe"; fi
    return
  fi
  if [ ! -f "$exe" ] || [ ! -x "$exe" ]; then
    echo "desktop: no program $1" >&2
    return 127
  fi
  shift
  uid=$(id -u)
  tmp=$(getconf DARWIN_USER_TEMP_DIR)
  launchctl print "gui/$uid" | awk '$1 == 0 && $3 ~ /^trivm\.desktop\./ { print $3 }' |
    while read -r x; do
      [ -d "$tmp/$x" ] || launchctl bootout "gui/$uid/$x" 2>/dev/null || :
    done
  d=$(mktemp -d "$tmp/trivm.desktop.XXXXXXXXXX") || return
  job=${d##*/}
  cat > "$d/job.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>Label</key><string>$job</string>
<key>ProgramArguments</key><array/>
<key>EnvironmentVariables</key><dict/>
<key>ProcessType</key><string>Interactive</string>
<key>AbandonProcessGroup</key><true/>
<key>RunAtLoad</key><true/>
</dict></plist>
EOF
  for x in "$exe" "$@"; do
    plutil -insert ProgramArguments -string "$x" -append "$d/job.plist" || s=1
  done
  [ $s -eq 0 ] && plutil -insert WorkingDirectory -string "$PWD" "$d/job.plist" &&
    plutil -insert EnvironmentVariables.PATH -string "$PATH" "$d/job.plist" &&
    launchctl bootstrap "gui/$uid" "$d/job.plist" || { rm -rf "$d"; return 1; }
  # The domain's line of the job: its pid and last status, not its arguments, which may say
  # anything. A pid may be there before the program fails to run: it counts once seen for 0.2 s.
  while [ $n -lt 100 ]; do
    s=$(launchctl print "gui/$uid" | awk -v job="$job" '$3 == job { print $1, $2 }')
    case $s in
      '' | '0 -') ;;
      0\ *) s=${s#0 } && break ;;
      *) up=$((up + 1)) && [ $up -eq 3 ] && s=up && break ;;
    esac
    n=$((n + 1))
    sleep 0.1
  done
  [ "$s" = up ] || launchctl bootout "gui/$uid/$job"
  rm -rf "$d"
  case $s in
    up | 0) return 0 ;;
    '' | '0 -') echo "desktop: $exe didn't start in 10 seconds" >&2 ;;
    78) echo "desktop: $exe exited: 78, launchd's EX_CONFIG when it can't run it" >&2 ;;
    -*) echo "desktop: $exe ended on signal ${s#-}" >&2 ;;
    *) echo "desktop: $exe exited: $s" >&2 ;;
  esac
  return 1
}
"#;

/// `desktop <program> [args]` on Linux: a service of the user's systemd, with the desktop's
/// environment (sshd's `DISPLAY` if it gave none), arguments' `$` doubled against systemd's
/// expansion, forks kept; a failed unit is reset once read, or 10 s on by a later call.
const LINUX_DESKTOP: &str = r#"desktop() {
  if [ $# -eq 0 ]; then
    echo 'usage: desktop <program> [args]' >&2
    return 2
  fi
  local exe=$1 unit k v state= code= status=
  case $exe in
    */*) ;;
    *) exe=$(type -P -- "$exe") || exe= ;;
  esac
  case $exe in
    /*) ;;
    ?*) exe=$PWD/$exe ;;
  esac
  if [ ! -f "$exe" ] || [ ! -x "$exe" ]; then
    echo "desktop: no program $1" >&2
    return 127
  fi
  shift
  set -- "$exe" "${@//\$/\$\$}"
  if ! systemctl --user show-environment | grep -Eq '^(WAYLAND_)?DISPLAY='; then
    set -- -E "DISPLAY=$DISPLAY" "$@"
  fi
  unit=trivm-desktop-$(date +%s%N)
  systemctl --user list-units --state=failed --plain --no-legend 'trivm-desktop-*' |
    while read -r k v; do
      k=${k#trivm-desktop-}
      if [ "${k%.service}" -lt "$((${unit#trivm-desktop-} - 10000000000))" ]; then
        systemctl --user reset-failed "trivm-desktop-$k" 2>/dev/null || true
      fi
    done
  systemd-run --user --quiet --unit="$unit" --service-type=exec --same-dir -E "PATH=$PATH" \
    -p KillMode=process -p StandardOutput=null -p StandardError=null "$@" || {
    systemctl --user reset-failed "$unit" 2>/dev/null || true
    echo "desktop: $exe couldn't be executed" >&2
    return 1
  }
  sleep 0.2
  while IFS== read -r k v; do
    case $k in
      ActiveState) state=$v ;;
      ExecMainCode) code=$v ;;
      ExecMainStatus) status=$v ;;
    esac
  done <<EOF
$(systemctl --user show -p ActiveState -p ExecMainCode -p ExecMainStatus "$unit")
EOF
  [ "$state" = active ] && return
  systemctl --user reset-failed "$unit" 2>/dev/null || true
  case $code:$status in
    *:0) return 0 ;;
    1:*) echo "desktop: $exe exited: $status" >&2 ;;
    *) echo "desktop: $exe ended on signal $status" >&2 ;;
  esac
  return 1
}
"#;

/// `exec`'s command line: `desktop` defined, then the command.
fn exec_line(os: Os, cmd: &[String]) -> String {
  match os {
    Os::Macos => format!("{MACOS_DESKTOP}{}", line(cmd)),
    Os::Linux => format!("{LINUX_DESKTOP}{}", line(cmd)),
    Os::Windows => windows::line(cmd),
  }
}

pub fn exec(vm: &Vm, cmd: &[String]) -> io::Result<ExitStatus> {
  let line = exec_line(vm.config.os(), cmd);
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
    // zsh expands a word that starts with `=`; a first word with one sets a variable.
    assert_eq!(
      line(&v(&["echo", "a b", "it's", "x=1", "=ls"])),
      r"echo 'a b' 'it'\''s' 'x=1' '=ls'"
    );
    assert_eq!(quote(""), "''");
  }

  #[test]
  fn desktop() {
    // Each system's `desktop` in its shell, refusing no program and a missing one.
    let cmd = vec!["desktop; echo $?; desktop /nope 2>&1; echo $?".to_string()];
    for (shell, os) in [("zsh", Os::Macos), ("bash", Os::Linux)] {
      let line = exec_line(os, &cmd);
      assert!(line.ends_with(&format!("}}\n{}", cmd[0])));
      let out = Command::new(shell).args(["-c", &line]).output().unwrap();
      let out = String::from_utf8_lossy(&out.stdout);
      assert_eq!(out, "2\ndesktop: no program /nope\n127\n", "{shell}");
    }
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
