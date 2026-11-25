// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! The systems trivm installs, each from its own source pinned by SHA-256, and `create`: the
//! download, the VM's files, the install, and the first boot that makes the user.

use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use std::{fs, thread};

use crate::vm::{self, Config, Os, PASSWORD, USER, Vm};
use crate::{apfs, assistant, bad, qcow2, qemu, run, ssh, vz, windows};

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
  pub desktop: Desktop,
  /// A Linux system's packages for its desktop, installed by cloud-init on the first boot; with
  /// `xprop` and `xwininfo`, which `click --name` reads X's windows with.
  pub packages: &'static [&'static str],
}

/// The desktop a system starts to, logged in automatically; Linux's on X11, so an app started
/// over SSH shows on `DISPLAY=:0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Desktop {
  /// The system's own: macOS's, Windows'.
  Own,
  /// Xfce with LightDM.
  Xfce,
  /// KDE Plasma with SDDM.
  Plasma,
  /// GNOME with GDM, Wayland off.
  Gnome,
}

impl System {
  /// macOS from an IPSW of Apple's.
  const fn macos(name: &'static str, url: &'static str, sha256: &'static str) -> Self {
    let (os, desktop) = (Os::Macos, Desktop::Own);
    Self { name, os, url, sha256, cpus: 4, memory: 8, disk: 64, desktop, packages: &[] }
  }

  /// Linux from a distro's cloud image.
  const fn linux(
    name: &'static str,
    url: &'static str,
    sha256: &'static str,
    desktop: Desktop,
    packages: &'static [&'static str],
  ) -> Self {
    Self { name, os: Os::Linux, url, sha256, cpus: 4, memory: 4, disk: 32, desktop, packages }
  }
}

pub const SYSTEMS: &[System] = &[
  System::macos(
    "macos-27",
    "https://updates.cdn-apple.com/2026FallFCS/c9671975-52db-4fb5-9af2-00217169ba8d/UniversalMac_27.2_26B123_Restore.ipsw",
    "6613b34e8589db2202a01ec37a2648afb141ece13428175183f8f3e38438534f",
  ),
  // The last IPSW of each older macOS: Apple publishes none for the security updates after it.
  System::macos(
    "macos-26",
    "https://updates.cdn-apple.com/2026SummerFCS/fullrestores/140-75212/A2A24B94-1FC1-45A3-93F7-C51B02AF1F4D/UniversalMac_26.6.2_25G83_Restore.ipsw",
    "885503b7f4b06609e9a512f2befd40f59730640a3f1233e3892d60affdd51c95",
  ),
  System::macos(
    "macos-15",
    "https://updates.cdn-apple.com/2025SummerFCS/fullrestores/093-10809/CFD6DD38-DAF0-40DA-854F-31AAD1294C6F/UniversalMac_15.6.1_24G90_Restore.ipsw",
    "3d87686b691ac765eb6a6b3082b2334e2af9710096a00432dd519af89ff2ea78",
  ),
  System::macos(
    "macos-14",
    "https://updates.cdn-apple.com/2024SummerFCS/fullrestores/062-52859/932E0A8F-6644-4759-82DA-F8FA8DEA806A/UniversalMac_14.6.1_23G93_Restore.ipsw",
    "e387251e63387150ba76cf784bdb97d2016e5d43c6f809c90a115eb067a097b8",
  ),
  System::macos(
    "macos-13",
    "https://updates.cdn-apple.com/2023FallFCS/fullrestores/042-55833/C0830847-A2F8-458F-B680-967991820931/UniversalMac_13.6_22G120_Restore.ipsw",
    "9bf095739b8b2d5ebd20f7e8de938f10bc449f9843de21c4a41ae54d73526728",
  ),
  System::linux(
    "ubuntu-24.04",
    "https://cloud-images.ubuntu.com/releases/noble/release-20261017/ubuntu-24.04-server-cloudimg-arm64.img",
    "50e24e1fb79e89116e939d90aa7738f7af48c442fa1bc2510131e0b39c50348e",
    Desktop::Xfce,
    &[
      "xfce4",
      "xfce4-terminal",
      "lightdm",
      "lightdm-gtk-greeter",
      "xserver-xorg",
      "dbus-x11",
      "x11-utils",
    ],
  ),
  System::linux(
    "ubuntu-22.04",
    "https://cloud-images.ubuntu.com/releases/jammy/release-20261109/ubuntu-22.04-server-cloudimg-arm64.img",
    "e429865ad84a083a84fdbd51bf6fd6e4174bbb1610ba0d72ca01014c656d45bc",
    Desktop::Plasma,
    &["kde-plasma-desktop", "sddm", "xserver-xorg", "x11-utils"],
  ),
  // `generic`, not `genericcloud`: the cloud kernel has no DRM, USB or HID, so no screen or input.
  System::linux(
    "debian-13",
    "https://cloud.debian.org/images/cloud/trixie/20261123-2671/debian-13-generic-arm64-20261123-2671.qcow2",
    "c7e96fd672cfd7af9ab4412468d911bd7ff5d7b15cbe94ffa4c5df2c72e659c2",
    Desktop::Gnome,
    &[
      "gdm3",
      "gnome-session-xsession",
      "gnome-shell",
      "gnome-terminal",
      "nautilus",
      "xserver-xorg",
      "x11-utils",
    ],
  ),
  System::linux(
    "fedora-45",
    "https://dl.fedoraproject.org/pub/fedora/linux/releases/45/Cloud/aarch64/images/Fedora-Cloud-Base-Generic-45-1.4.aarch64.qcow2",
    "1632acc8bc14d45b6c9596a0acc71b070d3748d1abdcf1a519be5c983bb0286c",
    Desktop::Xfce,
    &[
      "@xfce-desktop",
      "lightdm",
      "lightdm-gtk",
      "xorg-x11-server-Xorg",
      "dbus-x11",
      "xprop",
      "xwininfo",
    ],
  ),
  System {
    name: "windows-11",
    os: Os::Windows,
    // Its page's flow gives a link only to a browser: the person downloads the ISO, `--iso`.
    url: "https://www.microsoft.com/en-us/software-download/windows11arm64",
    // The page's list, English: Windows 11 26H2 (26300.9457) Arm64.
    sha256: "8992f2d2cfd2fa647e579e9eb31fc21f34da8333c29b6170fde17d2ed6623e9c",
    cpus: 4,
    memory: 6,
    disk: 64,
    desktop: Desktop::Own,
    packages: &[],
  },
];

/// Windows' ISO in the cache, cloned there from `--iso`.
const WINDOWS_ISO: &str = "Win11_26H2_English_Arm64.iso";

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

/// The CDs of a Windows VM being set up: Microsoft's ISO and the seed.
pub fn windows_cds(vm: &Vm) -> Vec<PathBuf> {
  let cds = [vm::cache_dir().join(WINDOWS_ISO), vm.path("seed.iso")];
  match vm.setup {
    true => cds.into_iter().filter(|x| x.exists()).collect(),
    false => Vec::new(),
  }
}

/// Windows' ISO in the cache, cloned there from `iso` once its SHA-256 is Microsoft's.
fn windows_download(sys: &System, iso: Option<&str>) -> io::Result<PathBuf> {
  let path = vm::cache_dir().join(WINDOWS_ISO);
  vm::made(&vm::cache_dir())?;
  let _lock = lock(&path)?;
  if path.exists() {
    return Ok(path);
  }

  let Some(iso) = iso else {
    return Err(bad(format!(
      "Microsoft gives its ISO only to a browser: download Windows 11 (multi-edition ISO for \
      Arm64), English, from {}, then: trivm create {} --iso <path>",
      sys.url, sys.name
    )));
  };

  check(Path::new(iso), sys.sha256)
    .map_err(|e| bad(format!("{e}: not Microsoft's English Arm64 ISO of 26H2")))?;
  let part = path.with_extension("part");
  if run(Command::new("cp").arg("-c").arg(iso).arg(&part)).is_err() {
    run(Command::new("cp").arg(iso).arg(&part))?;
  }

  fs::rename(&part, &path)?;
  Ok(path)
}

/// A lock for a file in the cache, in trivm's locks, held until it is dropped: a second create of
/// the same system waits for the first one's download or conversion instead of writing the same
/// file.
fn lock(path: &Path) -> io::Result<fs::File> {
  let name = path.with_extension("lock");
  let f = fs::File::create(vm::locks_dir()?.join(name.file_name().unwrap_or_default()))?;
  f.lock()?;
  Ok(f)
}

pub fn create(
  system: &str,
  name: Option<String>,
  cpus: Option<u32>,
  memory: Option<u32>,
  disk: Option<u32>,
  iso: Option<String>,
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
    Os::Windows => windows_download(sys, iso.as_deref())?,
    _ if iso.is_some() => {
      return Err(bad(format!("--iso: {} installs from its own image", sys.name)));
    }
    _ => download(sys.url, sys.sha256)?,
  };

  // Held while installing, so the VM isn't deleted or started in the middle; listed as new.
  vm.claim()?;
  let made = vm.hold().and_then(|hold| {
    vm.commit()?;
    match sys.os {
      Os::Macos => macos(&vm, &file)?,
      Os::Linux => linux(&vm, sys, &file)?,
      Os::Windows => windows(&vm)?,
    }

    Ok(hold)
  });

  // A VM whose install failed is no use: it goes, its downloads stay.
  match made {
    Ok(hold) => drop(hold),
    Err(e) => {
      let _ = vm.remove();
      return Err(e);
    }
  }

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
  // VZ finds no IPSW through a symlink on its path, such as a cache folder linked to another.
  let ipsw = &fs::canonicalize(ipsw)?;
  sparse(&vm.path("disk.img"), vm.config.disk_bytes())?;
  vz::mac_platform(vm, ipsw)?;
  eprintln!("installing macOS on {} (about 10 minutes)", vm.name);
  vz::install(vm, ipsw)?;
  match vm.config.provisioned() {
    true => Ok(()),
    // Its first boot shows the Setup Assistant, which trivm answers.
    false if apfs::encrypted(&vm.path("disk.img"), apfs::DATA)? => {
      fs::write(vm.path(assistant::MARK), "")
    }
    false => mac_data(vm),
  }
}

/// The files an older macOS's first boot reads, relative to its Data volume: the Setup Assistant
/// done, and a LaunchDaemon that runs the setup script as root. launchd loads a daemon only when
/// root owns it.
const MAC_SETUP_DONE: &str = "private/var/db/.AppleSetupDone";
const MAC_SKIP_BUDDY: &str = "Library/User Template/.skipbuddy";
const MAC_DAEMON: &str = "Library/LaunchDaemons/trivm.setup.plist";
const MAC_SCRIPT: &str = "private/var/db/trivm-setup.sh";

/// Writes the first boot's files to the Data volume of a new macOS VM before 27, as Lima does
/// (`pkg/guestpatch/macos`): the disk attached, the volume mounted by this user, which ignores
/// ownership, the files written, then their owner set to root on the disk itself.
fn mac_data(vm: &Vm) -> io::Result<()> {
  let disk = vm.path("disk.img");
  let uuid = apfs::uuid(&disk, apfs::DATA)?;
  let out = run(Command::new("diskutil").args(["image", "attach", "--noMount"]).arg(&disk))?;
  let out = String::from_utf8_lossy(&out);
  let dev = out.split_whitespace().next().ok_or_else(|| bad("diskutil attached no disk"))?;

  let mnt = vm.path("data");
  let written = mac_write(&uuid, &mnt);
  let _ = fs::remove_dir(&mnt);
  run(Command::new("hdiutil").args(["detach", "-force", dev]))?;
  written?;

  apfs::chown_root(&disk, apfs::DATA, &[MAC_SETUP_DONE, MAC_SKIP_BUDDY, MAC_DAEMON, MAC_SCRIPT])
}

fn mac_write(uuid: &str, mnt: &Path) -> io::Result<()> {
  let info = run(Command::new("diskutil").args(["info", uuid]))?;
  let info = String::from_utf8_lossy(&info);
  let id = info.lines().find_map(|x| x.trim().strip_prefix("Device Identifier:"));
  let id = id.map(str::trim).ok_or_else(|| bad(format!("diskutil: no volume {uuid}")))?;

  fs::create_dir_all(mnt)?;
  run(
    Command::new("mount")
      .args(["-t", "apfs", "-o", "noowners,nobrowse"])
      .arg(format!("/dev/{id}"))
      .arg(mnt),
  )?;

  let written = (|| {
    fs::write(mnt.join(MAC_SETUP_DONE), "")?;
    fs::write(mnt.join(MAC_SKIP_BUDDY), "")?;
    fs::write(mnt.join(MAC_DAEMON), mac_daemon())?;
    fs::write(mnt.join(MAC_SCRIPT), mac_script())
  })();

  run(Command::new("umount").arg(mnt))?;
  written
}

fn mac_daemon() -> String {
  format!(
    r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>trivm.setup</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/sh</string>
    <string>/{MAC_SCRIPT}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>StandardOutPath</key>
  <string>/var/log/trivm-setup.log</string>
  <key>StandardErrorPath</key>
  <string>/var/log/trivm-setup.log</string>
</dict>
</plist>
"#
  )
}

/// Run by the LaunchDaemon at the first two boots: the user, the first login's screens marked seen
/// (Lima's `com.apple.SetupAssistant.plist`, Apache-2.0) and the automatic login, then a restart,
/// as loginwindow logs in by itself only at boot; then Remote Login, so SSH answers after all that.
fn mac_script() -> String {
  let kc = kcpassword(PASSWORD);
  format!(
    r#"set -x
if id {USER}; then
  launchctl load -w /System/Library/LaunchDaemons/ssh.plist
  rm -f /{MAC_DAEMON} /{MAC_SCRIPT}
  exit
fi

for i in $(seq 60); do
  sysadminctl -addUser {USER} -fullName User -password {PASSWORD} -admin
  id {USER} && break
  sleep 2
done

createhomedir -c -u {USER}
prefs=/Users/{USER}/Library/Preferences
mkdir -p $prefs
version=$(sw_vers -productVersion)
build=$(sw_vers -buildVersion)
cat > $prefs/com.apple.SetupAssistant.plist <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>DidSeeAccessibility</key><true/>
  <key>DidSeeActivationLock</key><true/>
  <key>DidSeeAppStore</key><true/>
  <key>DidSeeAppearanceSetup</key><true/>
  <key>DidSeeApplePaySetup</key><true/>
  <key>DidSeeCloudSetup</key><true/>
  <key>DidSeeLockdownMode</key><true/>
  <key>DidSeePrivacy</key><true/>
  <key>DidSeeScreenTime</key><true/>
  <key>DidSeeSetupSequence</key><true/>
  <key>DidSeeSiriSetup</key><true/>
  <key>DidSeeSyncSetup</key><true/>
  <key>DidSeeSyncSetup2</key><true/>
  <key>DidSeeTermsOfAddress</key><true/>
  <key>DidSeeTouchIDSetup</key><true/>
  <key>DidSeeiCloudLoginForStorageServices</key><true/>
  <key>LastPreLoginTasksPerformedBuild</key><string>$build</string>
  <key>LastPreLoginTasksPerformedVersion</key><string>$version</string>
  <key>LastSeenAgeRangeSelectionProductVersion</key><string>$version</string>
  <key>LastSeenBuddyBuildVersion</key><string>$build</string>
  <key>LastSeenCloudProductVersion</key><string>$version</string>
  <key>LastSeenDiagnosticsProductVersion</key><string>$version</string>
  <key>MiniBuddyLaunchReason</key><integer>0</integer>
  <key>MiniBuddyShouldLaunchToResumeSetup</key><false/>
  <key>SkipExpressSettingsUpdating</key><true/>
  <key>SkipFirstLoginOptimization</key><true/>
</dict>
</plist>
EOF
chown {USER}:staff $prefs/com.apple.SetupAssistant.plist

printf '{kc}' > /etc/kcpassword
chmod 600 /etc/kcpassword
defaults write /Library/Preferences/com.apple.loginwindow autoLoginUser {USER}
shutdown -r now
"#
  )
}

/// `/etc/kcpassword`, the automatic login's password, in `printf`'s octal escapes: NUL-terminated,
/// padded with NULs to a multiple of 12 bytes, XORed with loginwindow's key, as `sysadminctl
/// -autologin set` writes it (which fails at the first boot, `SACSetAutoLoginPassword error:22`).
fn kcpassword(password: &str) -> String {
  const KEY: [u8; 11] = [0x7d, 0x89, 0x52, 0x23, 0xd2, 0xbc, 0xdd, 0xea, 0xa3, 0xb9, 0x1f];
  let mut b = password.as_bytes().to_vec();
  b.push(0);
  b.resize(b.len().div_ceil(12) * 12, 0);
  b.iter().zip(KEY.iter().cycle()).map(|(x, k)| format!("\\{:03o}", x ^ k)).collect()
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
/// time zone, `$2`, which macOS would otherwise guess as Cupertino's; the automatic login of `$4`
/// by its `kcpassword`, `$3`, which the Setup Assistant leaves off.
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
printf "$3" | sudo -n sh -c 'umask 077; cat > /etc/kcpassword'
sudo -n defaults write /Library/Preferences/com.apple.loginwindow autoLoginUser "$4"
"#;

/// The first boot of a new macOS VM, which made the user, or whose Setup Assistant trivm answers
/// to make it: the key, then the tuning.
pub fn macos_setup(vm: &Vm) -> io::Result<()> {
  if vm.path(assistant::MARK).exists() {
    assistant::answer(vm)?;
  }

  eprintln!("waiting for macOS to make the user and answer SSH (a few minutes)");
  let script =
    format!("sh -ec {} enroll {} {USER}", ssh::quote(MAC_ENROLL), ssh::quote(&ssh::public_key()?));
  ssh::with_password(vm, &script, Duration::from_secs(1800))?;
  let _ = fs::remove_file(vm.path(assistant::MARK));
  ssh::wait(vm, Duration::from_secs(60))?;
  let kc = ssh::quote(&kcpassword(PASSWORD));
  let tune = format!("sh -ec {} tune {PASSWORD} {} {kc} {USER}", ssh::quote(MAC_TUNE), timezone());
  ssh::output(vm, &tune)?;
  Ok(())
}

// MARK: Linux

fn linux(vm: &Vm, sys: &System, qcow2: &Path) -> io::Result<()> {
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
  fs::write(seed.join("user-data"), user_data(sys, &ssh::public_key()?, &timezone()))?;
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

/// cloud-init's config: the desktop on X11, logged in automatically, never blanked, locked or
/// asleep; SSH sessions on its display, so an app started over SSH shows on it. The display
/// manager starts at the end, so no reboot.
fn user_data(sys: &System, key: &str, tz: &str) -> String {
  let (dm, files, run) = match sys.desktop {
    Desktop::Xfce => ("lightdm", XFCE.to_string(), String::new()),
    Desktop::Plasma => ("sddm", PLASMA.to_string(), String::new()),
    Desktop::Gnome => ("gdm3", [GNOME, NM].concat(), [GNOME_RUN, NM_RUN].concat()),
    Desktop::Own => ("", String::new(), String::new()),
  };

  let (files, run) = (files.replace("{user}", USER), run.replace("{user}", USER));
  let packages: Vec<_> =
    sys.packages.iter().chain(&["xdotool"]).map(|x| format!("'{x}'")).collect();
  let packages = packages.join(", ");
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
packages: [{packages}]
write_files:
  - path: /etc/ssh/sshd_config.d/50-trivm.conf
    content: |
      SetEnv DISPLAY=:0
{files}runcmd:
  - [sh, -c, "systemctl try-restart ssh || systemctl try-restart sshd"]
  - [systemctl, mask, sleep.target, suspend.target, hibernate.target, hybrid-sleep.target]
{run}  - [systemctl, set-default, graphical.target]
  - [systemctl, enable, {dm}]
  - [systemctl, start, --no-block, {dm}]
"#
  )
}

/// Fedora's Xfce comes with its screen saver, which locks, and its power manager, which blanks;
/// their defaults for every user are written after the packages, which bring some of their own.
const XFCE: &str = r#"  - path: /etc/lightdm/lightdm.conf.d/50-trivm.conf
    content: |
      [Seat:*]
      autologin-user={user}
      autologin-user-timeout=0
      autologin-session=xfce
      user-session=xfce
      xserver-command=X -s 0 -dpms
  - path: /etc/xdg/xfce4/xfconf/xfce-perchannel-xml/xfce4-screensaver.xml
    defer: true
    content: |
      <?xml version="1.0" encoding="UTF-8"?>
      <channel name="xfce4-screensaver" version="1.0">
        <property name="saver" type="empty">
          <property name="enabled" type="bool" value="false"/>
          <property name="idle-activation" type="empty">
            <property name="enabled" type="bool" value="false"/>
          </property>
        </property>
        <property name="lock" type="empty">
          <property name="enabled" type="bool" value="false"/>
        </property>
      </channel>
  - path: /etc/xdg/xfce4/xfconf/xfce-perchannel-xml/xfce4-power-manager.xml
    defer: true
    content: |
      <?xml version="1.0" encoding="UTF-8"?>
      <channel name="xfce4-power-manager" version="1.0">
        <property name="xfce4-power-manager" type="empty">
          <property name="dpms-enabled" type="bool" value="false"/>
          <property name="blank-on-ac" type="int" value="0"/>
        </property>
      </channel>
"#;

/// Plasma's defaults for every user: no screen locker, and a power profile with no actions, so
/// PowerDevil makes none of its own (it would dim and switch off the screen).
const PLASMA: &str = r#"  - path: /etc/sddm.conf.d/50-trivm.conf
    content: |
      [Autologin]
      User={user}
      Session=plasma
      [X11]
      ServerArguments=-nolisten tcp -s 0 -dpms
  - path: /etc/xdg/kscreenlockerrc
    content: |
      [Daemon]
      Autolock=false
      LockOnResume=false
  - path: /etc/xdg/powermanagementprofilesrc
    content: |
      [AC]
      icon=battery-charging
"#;

/// GDM's own config comes with its package, so it is written after the packages (`defer`).
/// GNOME Shell starts on its overview, which the autostart entry closes.
const GNOME: &str = r#"  - path: /etc/gdm3/daemon.conf
    defer: true
    content: |
      [daemon]
      AutomaticLoginEnable=true
      AutomaticLogin={user}
      WaylandEnable=false
  - path: /etc/dconf/profile/user
    content: |
      user-db:user
      system-db:local
  - path: /etc/dconf/db/local.d/00-trivm
    content: |
      [org/gnome/desktop/session]
      idle-delay=uint32 0
      [org/gnome/desktop/screensaver]
      lock-enabled=false
      [org/gnome/settings-daemon/plugins/power]
      sleep-inactive-ac-type='nothing'
  - path: /etc/xdg/autostart/trivm-desktop.desktop
    content: |
      [Desktop Entry]
      Type=Application
      Name=Desktop
      Exec=sh -c "for i in 1 2 3 4 5; do sleep 2; gdbus call -e -d org.gnome.Shell -o /org/gnome/Shell -m org.freedesktop.DBus.Properties.Set org.gnome.Shell OverviewActive '<false>'; done"
      OnlyShowIn=GNOME;
      NoDisplay=true
"#;

/// GNOME brings the user's D-Bus under systemd, which the user's systemd, up since trivm's first
/// SSH login, gets when it starts again: GNOME and SSH sessions then share `/run/user/<uid>/bus`.
const GNOME_RUN: &str = r#"  - [dconf, update]
  - [su, {user}, -c, "mkdir -p ~/.config && echo yes > ~/.config/gnome-initial-setup-done"]
  - [sh, -c, "systemctl restart user@$(id -u {user})"]
"#;

/// GNOME brings NetworkManager, which sees no network while netplan gives the NIC to networkd, so
/// apps asking it think they are offline: once it is installed, netplan hands the NIC over, on the
/// same lease. Plasma comes without it here, and netplan would then hand the NIC to nothing.
const NM: &str = r#"  - path: /etc/netplan/90-trivm.yaml
    defer: true
    permissions: "0600"
    content: |
      network:
        version: 2
        renderer: NetworkManager
"#;

const NM_RUN: &str = "  - [netplan, apply]\n";

/// Root's, as Fedora's `/run/cloud-init` is root's only. 2 is done with recoverable errors, such
/// as Fedora's hostname, which cloud-init sets only on a later try.
const CLOUD_INIT_WAIT: &str =
  "sudo cloud-init status --wait >/dev/null; [ $? != 1 ] || sudo cloud-init status --long";

/// The first boot of a new Linux VM: cloud-init done. The seed goes then, or the desktop would
/// show it as a disk from the next boot on.
pub fn linux_setup(vm: &Vm) -> io::Result<()> {
  eprintln!("waiting for cloud-init to install the desktop (several minutes)");
  ssh::wait(vm, Duration::from_secs(600))?;
  ssh::output(vm, CLOUD_INIT_WAIT)?;
  match fs::remove_file(vm.path("seed.iso")) {
    Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
    _ => Ok(()),
  }
}

// MARK: Windows

/// The disk, the firmware's variables and the seed: `autounattend.xml`, the setup script its first
/// logon runs, trivm's key, Win32-OpenSSH's MSI and virtio-net's driver from virtio-win.
fn windows(vm: &Vm) -> io::Result<()> {
  let virtio = download(VIRTIO.0, VIRTIO.1)?;
  let msi = download(OPENSSH.0, OPENSSH.1)?;
  sparse(&vm.path("disk.img"), vm.config.disk_bytes())?;
  qemu::platform(vm)?;

  let seed = vm.path("seed");
  fs::create_dir_all(&seed)?;
  // Windows' computer names are 15 characters at most.
  let host: String = hostname(&vm.name).chars().take(15).collect();
  fs::write(seed.join("autounattend.xml"), windows::autounattend(host.trim_end_matches('-')))?;
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

/// Whether a disk has a GPT, the first thing Windows Setup writes.
fn partitioned(disk: &Path) -> io::Result<bool> {
  let mut head = [0u8; 520];
  fs::File::open(disk)?.read_exact(&mut head)?;
  Ok(&head[512..] == b"EFI PART")
}

/// The first boot of a new Windows VM: the ISO's boot asks for a key first, then Setup installs
/// unattended, and the first logon's script starts sshd last. The CDs go then.
pub fn windows_setup(vm: &Vm) -> io::Result<()> {
  // Once Setup has partitioned the disk, no key: the prompt times out and the disk boots, so a
  // setup cut short goes on instead of wiping the disk again.
  if !partitioned(&vm.path("disk.img"))? {
    for _ in 0..40 {
      let _ = qemu::ask(vm, "key enter", "");
      thread::sleep(Duration::from_secs(1));
    }
  }

  eprintln!("installing Windows on {}, unattended", vm.name);
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
  let names = match (vm.config.os(), find(&vm.config.system)?.desktop) {
    // WallpaperAgent draws the desktop from macOS 14 on, the Dock before it.
    (Os::Macos, _) if vm.config.macos().is_some_and(|x| x < 14) => "Dock Finder",
    (Os::Macos, _) => "Dock Finder WallpaperAgent",
    (_, Desktop::Xfce) => "xfce4-session xfce4-panel xfdesktop",
    (_, Desktop::Plasma) => "ksmserver kwin_x11 plasmashell",
    (_, Desktop::Gnome) => "gnome-shell",
    _ => "",
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
  fn disks() {
    let path = std::env::temp_dir().join(format!("trivm-disk-{}", std::process::id()));
    let mut data = vec![0u8; 1024];
    fs::write(&path, &data).unwrap();
    assert!(!partitioned(&path).unwrap());

    data[512..520].copy_from_slice(b"EFI PART");
    fs::write(&path, &data).unwrap();
    assert!(partitioned(&path).unwrap());
    fs::remove_file(&path).unwrap();
  }

  #[test]
  fn systems() {
    assert_eq!(find("macos-27").unwrap().os, Os::Macos);
    assert_eq!(find("windows-11").unwrap().os, Os::Windows);
    assert!(find("windows-10").is_err());
    assert!(SYSTEMS.iter().all(|x| crate::args::HELP.contains(x.name)));
    for x in SYSTEMS.iter().map(|x| (x.url, x.sha256)).chain([VIRTIO, OPENSSH]) {
      assert_eq!(x.1.len(), 64, "{}", x.0);
      assert!(x.1.chars().all(|x| x.is_ascii_hexdigit() && !x.is_ascii_uppercase()), "{}", x.0);
      assert!(x.0.starts_with("https://"), "{}", x.0);
    }
  }

  fn lint(cmd: &str, args: &[&str], text: &str) -> bool {
    use std::io::Write;

    let mut child = Command::new(cmd).args(args).stdin(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
    child.wait().unwrap().success()
  }

  #[test]
  fn mac_files() {
    // The bytes that logged in on macOS 26.
    let kc = r"\114\273\141\027\322\274\335\352\243\271\037\175";
    assert_eq!(kcpassword("1234"), kc);
    assert_eq!(kcpassword("12345678901").len(), 12 * 4);
    assert_eq!(kcpassword("123456789012").len(), 24 * 4);

    let script = mac_script();
    assert!(script.contains(&format!("printf '{kc}' > /etc/kcpassword\n")));
    assert!(script.contains("rm -f /Library/LaunchDaemons/trivm.setup.plist /private/var/db/"));
    assert!(script.contains("shutdown -r now\n"));
    assert!(lint("sh", &["-n"], &script));
    assert!(lint("plutil", &["-lint", "-s", "-"], &mac_daemon()));
    assert!(mac_daemon().contains("<string>/private/var/db/trivm-setup.sh</string>"));
    assert!(lint("sh", &["-n"], MAC_TUNE));
  }

  #[test]
  fn seed() {
    let linux = SYSTEMS.iter().filter(|x| x.os == Os::Linux);
    for sys in linux {
      let text = user_data(sys, "ssh-ed25519 AAAA trivm", "Europe/Sofia");
      assert!(text.starts_with("#cloud-config\n"));
      assert!(text.contains(r#"ssh_authorized_keys: ["ssh-ed25519 AAAA trivm"]"#));
      assert!(text.contains("timezone: Europe/Sofia\n"));
      assert!(text.contains("SetEnv DISPLAY=:0\n"));
      assert!(text.contains("'xdotool']\n"), "{}", sys.name);
      assert!(!text.contains('\t') && !text.contains("{user}"), "{}", sys.name);

      let (dm, login, nm) = match sys.desktop {
        Desktop::Xfce => ("lightdm", "autologin-user=user\n", false),
        Desktop::Plasma => ("sddm", "[Autologin]\n      User=user\n", false),
        Desktop::Gnome => ("gdm3", "AutomaticLogin=user\n", true),
        Desktop::Own => unreachable!(),
      };

      assert!(text.contains(&format!("[systemctl, enable, {dm}]\n")), "{}", sys.name);
      assert!(text.contains(login), "{}", sys.name);
      assert_eq!(text.contains("[netplan, apply]\n"), nm, "{}", sys.name);
    }

    assert_eq!(
      meta_data("ubuntu-24.04", "02:aa:00:00:00:01"),
      "instance-id: ubuntu-24-04-02aa00000001\nlocal-hostname: ubuntu-24-04\n"
    );
    assert!(NETWORK_CONFIG.contains("driver: virtio_net\n"));
    assert!(NETWORK_CONFIG.contains("dhcp-identifier: mac\n"));
    assert!(!NETWORK_CONFIG.contains('\t'));
  }
}
