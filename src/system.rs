// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! The systems trivm installs, each from its own source pinned by SHA-256, and `create`: the
//! download, the VM's files, the install, and the first boot that makes the user.

use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use std::{fs, thread};

use crate::vm::{self, Config, Os, PASSWORD, USER, Vm};
use crate::{bad, qcow2, run, ssh, vz};

pub struct System {
  pub name: &'static str,
  pub os: Os,
  pub url: &'static str,
  pub sha256: &'static str,
  pub cpus: u32,
  /// GiB.
  pub memory: u32,
  /// GiB.
  pub disk: u32,
}

pub const SYSTEMS: &[System] = &[
  System {
    name: "macos-27",
    os: Os::Macos,
    url: "https://updates.cdn-apple.com/2026FallFCS/c9671975-52db-4fb5-9af2-00217169ba8d/UniversalMac_27.2_26B123_Restore.ipsw",
    sha256: "6613b34e8589db2202a01ec37a2648afb141ece13428175183f8f3e38438534f",
    cpus: 4,
    memory: 8,
    disk: 64,
  },
  System {
    name: "ubuntu-24.04",
    os: Os::Linux,
    url: "https://cloud-images.ubuntu.com/releases/noble/release-20261017/ubuntu-24.04-server-cloudimg-arm64.img",
    sha256: "50e24e1fb79e89116e939d90aa7738f7af48c442fa1bc2510131e0b39c50348e",
    cpus: 4,
    memory: 4,
    disk: 32,
  },
];

pub fn find(name: &str) -> io::Result<&'static System> {
  let names: Vec<_> = SYSTEMS.iter().map(|x| x.name).collect();
  let err = || bad(format!("unknown system {name}; trivm installs {}", names.join(", ")));
  SYSTEMS.iter().find(|x| x.name == name).ok_or_else(err)
}

/// The file at `url` in the cache, downloaded (resumed after a break) and checked first.
pub fn download(url: &str, sha256: &str) -> io::Result<PathBuf> {
  let name = url.rsplit('/').next().unwrap_or(url);
  let path = vm::cache_dir().join(name);
  vm::made(&vm::cache_dir())?;
  let _lock = lock(&path)?;
  if path.exists() {
    return Ok(path);
  }

  let part = path.with_extension("part");
  eprintln!("downloading {url}");
  let mut curl = Command::new("curl");
  curl.args(["-fL", "--retry", "5", "-C", "-", "-o"]).arg(&part).arg(url);
  curl.arg(if io::stderr().is_terminal() { "--progress-bar" } else { "-sS" });
  if !curl.status()?.success() {
    return Err(bad(format!("downloading {url} failed; run again to resume")));
  }

  eprintln!("checking {name}");
  let out = run(Command::new("shasum").args(["-a", "256"]).arg(&part))?;
  let sum = String::from_utf8_lossy(&out);
  if sum.split_whitespace().next() != Some(sha256) {
    fs::remove_file(&part)?;
    return Err(bad(format!("{name}: SHA-256 isn't {sha256}; removed, run again")));
  }

  fs::rename(&part, &path)?;
  Ok(path)
}

/// A lock beside a file in the cache, held until it is dropped: a second create of the same system
/// waits for the first one's download or conversion instead of writing the same file.
fn lock(path: &Path) -> io::Result<fs::File> {
  let f = fs::File::create(path.with_extension("lock"))?;
  f.lock()?;
  Ok(f)
}

pub fn create(
  system: &str,
  name: Option<String>,
  cpus: Option<u32>,
  memory: Option<u32>,
  disk: Option<u32>,
) -> io::Result<()> {
  let sys = find(system)?;
  let name = name.unwrap_or_else(|| sys.name.to_string());
  let config = Config {
    system: sys.name.into(),
    cpus: cpus.unwrap_or(sys.cpus),
    memory: memory.unwrap_or(sys.memory),
    disk: disk.unwrap_or(sys.disk),
  };
  let vm = Vm { mac: vm::random_mac()?, setup: true, ..Vm::at(&name, config) };
  if vm.conf.exists() {
    return Err(bad(format!("{name} exists; trivm delete {name} first")));
  }

  let file = download(sys.url, sys.sha256)?;
  // Held while installing, and while a failed install goes, so the VM isn't deleted or started in
  // the middle; listed as new.
  let hold = vm.claim()?;
  let made = vm.commit().and_then(|_| match sys.os {
    Os::Macos => macos(&vm, &file),
    Os::Linux => linux(&vm, &file),
  });

  // A VM whose install failed is no use: it goes, its downloads stay.
  if let Err(e) = made {
    let _ = vm.remove();
    return Err(e);
  }

  drop(hold);

  crate::start(&vm.name)
}

fn sparse(path: &Path, bytes: u64) -> io::Result<()> {
  let f = fs::OpenOptions::new().create(true).write(true).truncate(false).open(path)?;
  if f.metadata()?.len() < bytes {
    f.set_len(bytes)?;
  }

  Ok(())
}

// MARK: macOS

fn macos(vm: &Vm, ipsw: &Path) -> io::Result<()> {
  sparse(&vm.path("disk.img"), vm.config.disk_bytes())?;
  vz::mac_platform(vm, ipsw)?;
  eprintln!("installing macOS on {} (about 10 minutes)", vm.name);
  vz::install(vm, ipsw)
}

/// Run over the first SSH login with the password: trivm's key, sudo without a password, SSH
/// with keys only. `$1` is the key; sudo reads the password from stdin.
const MAC_ENROLL: &str = r#"umask 077
mkdir -p "$HOME/.ssh"
printf '%s\n' "$1" > "$HOME/.ssh/authorized_keys"
sudo -S -p '' sh -ec "printf '%s\n' '$2 ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/trivm
chmod 440 /etc/sudoers.d/trivm
printf '%s\n' 'PasswordAuthentication no' 'KbdInteractiveAuthentication no' > /etc/ssh/sshd_config.d/000-trivm.conf"
"#;

/// No sleep, screen saver or lock, which would leave the screen dark; no automatic updates
/// (macOS 27 kept checking regardless of some of these, as the old `vm` tool found); the host's
/// time zone, `$2`, which macOS would otherwise guess as Cupertino's.
const MAC_TUNE: &str = r#"set -e
sudo -n pmset -a displaysleep 0 sleep 0 disksleep 0 >/dev/null
defaults -currentHost write com.apple.screensaver idleTime -int 0
sudo -n sysadminctl -screenLock off -password "$1" 2>/dev/null || true
for key in AutomaticCheckEnabled AutomaticDownload CriticalUpdateInstall ConfigDataInstall; do
  sudo -n defaults write /Library/Preferences/com.apple.SoftwareUpdate $key -bool false
done
sudo -n defaults write /Library/Preferences/com.apple.commerce AutoUpdate -bool false
sudo -n defaults write /Library/Preferences/com.apple.timezone.auto Active -bool false
sudo -n ln -sf "/var/db/timezone/zoneinfo/$2" /etc/localtime
"#;

/// The first boot of a new macOS VM, which made the user: the key, then the tuning.
pub fn macos_setup(vm: &Vm) -> io::Result<()> {
  eprintln!("waiting for macOS to make the user and answer SSH (a few minutes)");
  let script =
    format!("sh -ec {} enroll {} {USER}", ssh::quote(MAC_ENROLL), ssh::quote(&ssh::public_key()?));
  ssh::with_password(vm, &script, Duration::from_secs(1800))?;
  ssh::wait(vm, Duration::from_secs(60))?;
  ssh::output(vm, &format!("sh -ec {} tune {PASSWORD} {}", ssh::quote(MAC_TUNE), timezone()))?;
  Ok(())
}

// MARK: Linux

fn linux(vm: &Vm, qcow2: &Path) -> io::Result<()> {
  let image = qcow2.with_extension("raw");
  let _lock = lock(&image)?;
  if !image.exists() {
    eprintln!("converting {} to a raw disk", qcow2.display());
    let tmp = image.with_extension("part");
    qcow2::to_raw(qcow2, &tmp)?;
    fs::rename(&tmp, &image)?;
  }

  // An APFS clone: instant, and the image stays as it was.
  run(Command::new("cp").arg("-c").arg(&image).arg(vm.path("disk.img")))?;
  sparse(&vm.path("disk.img"), vm.config.disk_bytes())?;
  vz::linux_platform(vm)?;

  let seed = vm.path("seed");
  fs::create_dir_all(&seed)?;
  fs::write(seed.join("user-data"), user_data(&ssh::public_key()?, &timezone()))?;
  fs::write(seed.join("meta-data"), meta_data(&vm.name, &vm.mac))?;
  fs::write(seed.join("network-config"), NETWORK_CONFIG)?;
  let mut hdiutil = Command::new("hdiutil");
  hdiutil.args(["makehybrid", "-quiet", "-iso", "-joliet", "-default-volume-name", "cidata", "-o"]);
  run(hdiutil.arg(vm.path("seed.iso")).arg(&seed).stdin(Stdio::null()))?;
  fs::remove_dir_all(&seed)
}

fn hostname(name: &str) -> String {
  name.chars().map(|x| if x.is_ascii_alphanumeric() { x } else { '-' }).collect()
}

fn meta_data(name: &str, mac: &str) -> String {
  let host = hostname(name);
  format!("instance-id: {host}-{}\nlocal-hostname: {host}\n", mac.replace(':', ""))
}

/// DHCP on the virtio NIC, matched by its driver: a clone has another MAC address than the one
/// the VM was made with. The MAC address as the client's: Ubuntu's networkd sends a DUID by
/// default, which bootpd writes in its leases in place of the address trivm looks the guest up by.
const NETWORK_CONFIG: &str = r#"version: 2
ethernets:
  nic:
    match:
      driver: virtio_net
    dhcp4: true
    dhcp-identifier: mac
"#;

/// cloud-init's config: the user with trivm's key, Xfce on X11 logged in automatically, the
/// screen never blanked, the host's time zone, SSH sessions on the desktop's display (an app
/// started over SSH shows on it); LightDM is started at the end, so no reboot.
fn user_data(key: &str, tz: &str) -> String {
  format!(
    r#"#cloud-config
users:
  - name: {USER}
    gecos: User
    shell: /bin/bash
    groups: [adm, sudo, autologin]
    sudo: ALL=(ALL) NOPASSWD:ALL
    lock_passwd: false
    plain_text_passwd: "{PASSWORD}"
    ssh_authorized_keys: ["{key}"]
groups: [autologin]
ssh_pwauth: false
timezone: {tz}
package_update: true
packages: [xfce4, xfce4-terminal, lightdm, lightdm-gtk-greeter, xserver-xorg, dbus-x11, xdotool]
write_files:
  - path: /etc/lightdm/lightdm.conf.d/50-trivm.conf
    content: |
      [Seat:*]
      autologin-user={USER}
      autologin-user-timeout=0
      autologin-session=xfce
      user-session=xfce
      xserver-command=X -s 0 -dpms
  - path: /etc/ssh/sshd_config.d/50-trivm.conf
    content: |
      SetEnv DISPLAY=:0
runcmd:
  - [systemctl, try-restart, ssh]
  - [systemctl, set-default, graphical.target]
  - [systemctl, start, --no-block, lightdm]
"#
  )
}

/// The first boot of a new Linux VM: cloud-init done. The seed goes then, or the desktop would
/// show it as a disk from the next boot on.
pub fn linux_setup(vm: &Vm) -> io::Result<()> {
  eprintln!("waiting for cloud-init to install the desktop (several minutes)");
  ssh::wait(vm, Duration::from_secs(600))?;
  ssh::output(vm, "cloud-init status --wait >/dev/null || cloud-init status --long")?;
  match fs::remove_file(vm.path("seed.iso")) {
    Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
    _ => Ok(()),
  }
}

// MARK: Both

/// Waits for the desktop: its processes running, then the screen lit, which takes macOS up to
/// half a minute more while it is busy starting.
pub fn desktop(vm: &Vm) -> io::Result<()> {
  let names = match vm.config.os() {
    Os::Macos => "Dock Finder WallpaperAgent",
    Os::Linux => "xfce4-session xfce4-panel xfdesktop",
  };

  let script = format!(
    "for i in $(seq 150); do up=1; for x in {names}; do pgrep -u {USER} -x $x >/dev/null || up=; \
    done; [ $up ] && exit 0; sleep 2; done; echo 'no desktop in 5 minutes'; exit 1"
  );
  ssh::output(vm, &script)?;

  // A dark wallpaper would never count as lit, hence no error.
  for _ in 0..60 {
    let lit = vz::ask(vm, "lit", "").ok().and_then(|x| x.parse::<f64>().ok());
    if lit.is_some_and(|x| x > 0.5) {
      break;
    }

    thread::sleep(Duration::from_secs(1));
  }

  thread::sleep(Duration::from_secs(2));
  Ok(())
}

/// The host's time zone, such as `Europe/Sofia`, for the guest.
fn timezone() -> String {
  let link = fs::read_link("/etc/localtime").unwrap_or_default();
  let link = link.to_string_lossy();
  link.split_once("zoneinfo/").map_or("UTC", |x| x.1).to_string()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn systems() {
    assert_eq!(find("macos-27").unwrap().os, Os::Macos);
    assert!(find("windows-11").is_err());
    for x in SYSTEMS {
      assert_eq!(x.sha256.len(), 64, "{}", x.name);
      assert!(x.url.starts_with("https://"), "{}", x.name);
    }
  }

  #[test]
  fn seed() {
    let text = user_data("ssh-ed25519 AAAA trivm", "Europe/Sofia");
    assert!(text.starts_with("#cloud-config\n"));
    assert!(text.contains(r#"ssh_authorized_keys: ["ssh-ed25519 AAAA trivm"]"#));
    assert!(text.contains("autologin-user=user\n"));
    assert!(text.contains("timezone: Europe/Sofia\n"));
    assert!(text.contains("SetEnv DISPLAY=:0\n"));
    assert!(!text.contains('\t'));
    assert_eq!(
      meta_data("ubuntu-24.04", "02:aa:00:00:00:01"),
      "instance-id: ubuntu-24-04-02aa00000001\nlocal-hostname: ubuntu-24-04\n"
    );
    assert!(NETWORK_CONFIG.contains("driver: virtio_net\n"));
    assert!(NETWORK_CONFIG.contains("dhcp-identifier: mac\n"));
    assert!(!NETWORK_CONFIG.contains('\t'));
  }
}
