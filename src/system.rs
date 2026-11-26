// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! The systems trivm installs, each from its own source pinned by SHA-256, and `create`: the
//! download, the VM's files, the install, and the first boot that makes the user.

use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use std::{env, fs, thread};

use crate::vm::{self, Config, Os, PASSWORD, USER, Vm};
use crate::{bad, qcow2, qemu, run, ssh, vz, windows};

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
  System {
    name: "windows-11",
    os: Os::Windows,
    // Windows 11 24H2's consumer editions for arm64 in English, from the catalog of Microsoft's
    // Media Creation Tool (go.microsoft.com/fwlink/?LinkId=2156292). Its CDN has no certificate
    // for its name: HTTP, checked by the SHA-256.
    url: "http://dl.delivery.mp.microsoft.com/filestreamingservice/files/37acc189-7f4f-4e07-976c-1cf0a099ec79/26100.4349.250607-1500.ge_release_svc_refresh_CLIENTCONSUMER_RET_A64FRE_en-us.esd",
    sha256: "c01b6f474c772093159b63fb3a168d2666556b223d288f67efc37c4fbaeddd25",
    cpus: 4,
    memory: 6,
    disk: 64,
  },
];

/// Windows 11 Pro's image in the ESD, after Setup's media, Windows PE and Setup, 1 to 3.
const WINDOWS_PRO: u32 = 6;

/// Fedora's virtio-win, for NetKVM, the driver of the VM's virtio-net.
const VIRTIO: (&str, &str) = (
  "https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/archive-virtio/virtio-win-0.1.302-1/virtio-win-0.1.302.iso",
  "303f7ae40dad495d6ae474fdc571df58958a4dbc5c37a522d80f9a203867949d",
);

const OPENSSH: (&str, &str) = (
  "https://github.com/PowerShell/Win32-OpenSSH/releases/download/10.4.0.0p1-Preview/OpenSSH-ARM64-v10.4.0.0.msi",
  "75fa139d8975eedb706d9180f454e2148af09d417692825f383fe00610b00e21",
);

pub fn find(name: &str) -> io::Result<&'static System> {
  let names: Vec<_> = SYSTEMS.iter().map(|x| x.name).collect();
  let err = || bad(format!("unknown system {name}; trivm installs {}", names.join(", ")));
  SYSTEMS.iter().find(|x| x.name == name).ok_or_else(err)
}

/// A file in the cache, downloaded (resumed after a break) and checked first.
fn download(url: &str, sha256: &str) -> io::Result<PathBuf> {
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

  if let Err(e) = check(&part, sha256) {
    fs::remove_file(&part)?;
    return Err(bad(format!("{e}; removed, run again")));
  }

  fs::rename(&part, &path)?;
  Ok(path)
}

fn check(path: &Path, sha256: &str) -> io::Result<()> {
  let name = path.file_name().unwrap_or_default().to_string_lossy();
  eprintln!("checking {name}");
  let out = run(Command::new("shasum").args(["-a", "256"]).arg(path))?;
  match String::from_utf8_lossy(&out).split_whitespace().next() == Some(sha256) {
    true => Ok(()),
    false => Err(bad(format!("{name}: SHA-256 isn't {sha256}"))),
  }
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

  let file = match sys.os {
    Os::Windows => {
      tools()?;
      qemu::fits(&vm)?;
      windows_media(sys)?
    }
    _ => download(sys.url, sys.sha256)?,
  };

  // Held while installing, and while a failed install goes, so the VM isn't deleted or started in
  // the middle; listed as new.
  let hold = vm.claim()?;
  let made = vm.commit().and_then(|_| match sys.os {
    Os::Macos => macos(&vm, &file),
    Os::Linux => linux(&vm, &file),
    Os::Windows => windows(&vm),
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

/// The VM's name as Windows' computer name, which is 15 characters at most and not digits only.
fn computer_name(name: &str) -> String {
  let short = |x: &str| x.chars().take(15).collect::<String>().trim_matches('-').to_string();
  let host = short(hostname(name).trim_matches('-'));
  match host.chars().all(|x| x.is_ascii_digit()) {
    true => short(&format!("win-{host}")),
    false => host,
  }
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

// MARK: Windows

/// Fails unless Homebrew's tools for Windows are on `PATH`, before the downloads they need.
fn tools() -> io::Result<()> {
  let path = env::var_os("PATH").unwrap_or_default();
  for x in ["qemu-system-aarch64", "swtpm", "wimlib-imagex"] {
    if !env::split_paths(&path).any(|p| p.join(x).is_file()) {
      return Err(bad(format!("{x} not found; brew install qemu swtpm wimlib")));
    }
  }

  Ok(())
}

/// Windows' install media in the cache, named after the ESD it is made from.
fn media(sys: &System) -> PathBuf {
  let name = sys.url.rsplit('/').next().unwrap_or(sys.name);
  vm::cache_dir().join(name).with_extension("pro.iso")
}

/// Windows' install media, made once from the ESD with wimlib as the Media Creation Tool makes it:
/// image 1's files, Windows PE and Setup (2, 3) as `sources/boot.wim`, Pro as `install.esd`, in an
/// ISO booted by its El Torito image for UEFI, which asks for no key. The ESD goes then.
fn windows_media(sys: &System) -> io::Result<PathBuf> {
  let iso = media(sys);
  vm::made(&vm::cache_dir())?;
  let _lock = lock(&iso)?;
  if iso.exists() {
    return Ok(iso);
  }

  let esd = download(sys.url, sys.sha256)?;
  eprintln!("making Windows' install media from {}", esd.display());
  // A make cut short left its folder.
  let dir = iso.with_extension("part");
  let _ = fs::remove_dir_all(&dir);
  let wimlib = |cmd: &str, image: u32, to: &Path, opts: &[&str]| -> io::Result<()> {
    let mut c = Command::new("wimlib-imagex");
    run(c.arg(cmd).arg(&esd).arg(image.to_string()).arg(to).args(opts)).map(|_| ())
  };

  wimlib("apply", 1, &dir, &[])?;
  let boot = dir.join("sources/boot.wim");
  wimlib("export", 2, &boot, &["--compress=LZX"])?;
  wimlib("export", 3, &boot, &["--boot"])?;
  wimlib("export", WINDOWS_PRO, &dir.join("sources/install.esd"), &["--solid"])?;

  // UDF for install.esd, past ISO9660's 4 GiB; El Torito's image as its own sectors of 512 bytes.
  let efi = dir.join("efi/microsoft/boot/efisys_noprompt.bin");
  let sectors = fs::metadata(&efi)?.len() / 512;
  let part = iso.with_extension("part.iso");
  let mut hdiutil = Command::new("hdiutil");
  hdiutil.args(["makehybrid", "-quiet", "-iso", "-udf", "-default-volume-name", "WIN11_ARM64"]);
  hdiutil.arg("-eltorito-boot").arg(&efi).args(["-no-emul-boot", "-eltorito-platform", "0xEF"]);
  hdiutil.args(["-boot-load-size", &sectors.to_string(), "-ov", "-o"]);
  run(hdiutil.arg(&part).arg(&dir).stdin(Stdio::null()))?;
  fs::rename(&part, &iso)?;
  fs::remove_dir_all(&dir)?;
  fs::remove_file(&esd)?;
  Ok(iso)
}

/// The CDs of a Windows VM being set up: the install media and the seed.
pub fn windows_cds(vm: &Vm) -> io::Result<Vec<PathBuf>> {
  let cds = [media(find(&vm.config.system)?), vm.path("seed.iso")];
  Ok(match vm.setup {
    true => cds.into_iter().filter(|x| x.exists()).collect(),
    false => Vec::new(),
  })
}

/// The disk, the firmware's variables and the seed: `autounattend.xml`, the setup script its first
/// logon runs, trivm's key, Win32-OpenSSH's MSI and virtio-net's driver from virtio-win.
fn windows(vm: &Vm) -> io::Result<()> {
  let virtio = download(VIRTIO.0, VIRTIO.1)?;
  let msi = download(OPENSSH.0, OPENSSH.1)?;
  sparse(&vm.path("disk.img"), vm.config.disk_bytes())?;
  qemu::platform(vm)?;

  let seed = vm.path("seed");
  fs::create_dir_all(&seed)?;
  fs::write(seed.join("autounattend.xml"), windows::autounattend(&computer_name(&vm.name)))?;
  fs::write(seed.join(windows::SETUP), windows::setup_script())?;
  fs::write(seed.join("key.pub"), ssh::public_key()? + "\n")?;
  fs::copy(&msi, seed.join("openssh.msi"))?;

  // The ISO's files are hard links across NetKVM's folders, read-only once extracted.
  let tmp = vm.path("virtio");
  fs::create_dir_all(&tmp)?;
  run(Command::new("tar").arg("-xf").arg(&virtio).arg("-C").arg(&tmp).arg("NetKVM"))?;
  run(Command::new("chmod").arg("-R").arg("u+w").arg(&tmp))?;
  fs::rename(tmp.join("NetKVM/w11/ARM64"), seed.join("netkvm"))?;
  fs::remove_dir_all(&tmp)?;

  let mut hdiutil = Command::new("hdiutil");
  hdiutil.args(["makehybrid", "-quiet", "-iso", "-joliet", "-default-volume-name", "TRIVM", "-o"]);
  run(hdiutil.arg(vm.path("seed.iso")).arg(&seed).stdin(Stdio::null()))?;
  fs::remove_dir_all(&seed)
}

/// The first boot of a new Windows VM: Setup installs from the media unattended, restarting from
/// the disk, and the first logon's script starts sshd last. The CDs go then.
pub fn windows_setup(vm: &Vm) -> io::Result<()> {
  eprintln!("installing Windows on {}, unattended (about 15 minutes)", vm.name);
  ssh::wait(vm, Duration::from_secs(5400))?;
  qemu::ask(vm, "eject", "")?;
  match fs::remove_file(vm.path("seed.iso")) {
    Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
    _ => Ok(()),
  }
}

// MARK: All

/// Waits for the desktop: its processes running, then the screen lit, which takes macOS up to
/// half a minute more while it is busy starting.
pub fn desktop(vm: &Vm) -> io::Result<()> {
  let names = match vm.config.os() {
    Os::Macos => "Dock Finder WallpaperAgent",
    Os::Linux => "xfce4-session xfce4-panel xfdesktop",
    Os::Windows => "",
  };

  let script = match vm.config.os() {
    Os::Windows => windows::DESKTOP.to_string(),
    _ => format!(
      "for i in $(seq 150); do up=1; for x in {names}; do pgrep -u {USER} -x $x >/dev/null || up=; \
      done; [ $up ] && exit 0; sleep 2; done; echo 'no desktop in 5 minutes'; exit 1"
    ),
  };

  ssh::output(vm, &script)?;

  // A dark wallpaper would never count as lit, hence no error.
  for _ in 0..60 {
    let lit = crate::ask(vm, "lit", "").ok().and_then(|x| x.parse::<f64>().ok());
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
    assert_eq!(find("windows-11").unwrap().os, Os::Windows);
    assert!(find("windows-10").is_err());
    for x in SYSTEMS.iter().map(|x| (x.url, x.sha256)).chain([VIRTIO, OPENSSH]) {
      assert_eq!(x.1.len(), 64, "{}", x.0);
      assert!(x.1.chars().all(|x| x.is_ascii_hexdigit() && !x.is_ascii_uppercase()), "{}", x.0);
      let cdn = x.0.starts_with("http://dl.delivery.mp.microsoft.com/");
      assert!(x.0.starts_with("https://") || cdn, "{}", x.0);
    }

    // The media's lock isn't the ESD's, which its make takes too.
    let (sys, lock) = (find("windows-11").unwrap(), |x: &Path| x.with_extension("lock"));
    let esd = vm::cache_dir().join(sys.url.rsplit('/').next().unwrap());
    assert_eq!(esd.extension().unwrap(), "esd");
    assert_ne!(lock(&media(sys)), lock(&esd));
    assert!(media(sys).to_string_lossy().ends_with("_A64FRE_en-us.pro.iso"));
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

  #[test]
  fn computer_names() {
    for (name, host) in [
      ("windows-11", "windows-11"),
      ("windows-11-run-1", "windows-11-run"),
      ("my.test_vm-number-2", "my-test-vm-numb"),
      ("123", "win-123"),
      ("123456789012345a", "win-12345678901"),
      ("_", "win"),
      ("-x-", "x"),
    ] {
      assert_eq!(computer_name(name), host, "{name}");
    }
  }
}
