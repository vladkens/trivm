// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! trivm's folders, by the XDG Base Directory spec, and a VM's files in them: its config, its
//! disks, what its runs write; whether its process runs, and the guest's address from the host's
//! DHCP leases.

use std::ffi::OsString;
use std::io::{self, Read};
use std::os::unix::fs::{DirBuilderExt, FileExt};
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{env, fs, process, thread};

use crate::bad;

/// The guest's user and password, the same in every VM.
pub const USER: &str = "user";
pub const PASSWORD: &str = "1234";

const GIB: u64 = 1 << 30;

/// What a person sets: each VM's settings.
pub fn config_dir() -> PathBuf {
  xdg("XDG_CONFIG_HOME", ".config")
}

/// Each VM's disks, platform files and machine state, and trivm's SSH key.
pub fn data_dir() -> PathBuf {
  xdg("XDG_DATA_HOME", ".local/share")
}

/// Downloads and what is made from them: all of it can be made again.
pub fn cache_dir() -> PathBuf {
  xdg("XDG_CACHE_HOME", ".cache")
}

/// What VMs write as they run: logs, locks, sockets.
pub fn state_dir() -> PathBuf {
  xdg("XDG_STATE_HOME", ".local/state")
}

fn xdg(var: &str, default: &str) -> PathBuf {
  let home = PathBuf::from(env::var_os("HOME").unwrap_or_default());
  base(env::var_os(var), &home, default).join("trivm")
}

/// The folder an XDG variable names, or its default in the home folder when it is unset, empty
/// or not absolute, which the spec says to ignore.
fn base(var: Option<OsString>, home: &Path, default: &str) -> PathBuf {
  match var.map(PathBuf::from) {
    Some(x) if x.is_absolute() => x,
    _ => home.join(default),
  }
}

/// Makes a folder and those above it that are missing its owner's only (0700), as the spec asks:
/// a VM's disk holds its user's files, and the Mac's other users share the home folder's group.
pub fn made(dir: &Path) -> io::Result<()> {
  fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Os {
  Macos,
  Linux,
  Windows,
}

/// A VM's settings, the person's: `<vm>.conf` in trivm's config folder, `key = value` lines.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Config {
  pub system: String,
  pub cpus: u32,
  /// GiB.
  pub memory: u32,
  /// GiB.
  pub disk: u32,
}

impl Config {
  pub fn os(&self) -> Os {
    match self.system.split('-').next() {
      Some("macos") => Os::Macos,
      Some("windows") => Os::Windows,
      _ => Os::Linux,
    }
  }

  /// Whether VZ makes the guest's user on its first boot (`VZMacGuestProvisioningOptions`), as
  /// for macOS 27 and later; an older macOS gets its user from files on its Data volume, or from
  /// its Setup Assistant, which trivm answers, when that volume is encrypted.
  pub fn provisioned(&self) -> bool {
    self.macos().is_some_and(|x| x >= 27)
  }

  /// The version of a `macos-<n>` system.
  pub fn macos(&self) -> Option<u32> {
    self.system.strip_prefix("macos-").and_then(|x| x.parse().ok())
  }

  pub fn memory_bytes(&self) -> u64 {
    self.memory as u64 * GIB
  }

  pub fn disk_bytes(&self) -> u64 {
    self.disk as u64 * GIB
  }

  fn parse(text: &str) -> io::Result<Self> {
    let get = |key: &str| {
      let line = text.lines().find_map(|x| {
        let (k, v) = x.split_once('=')?;
        (k.trim() == key).then(|| v.trim().to_string())
      });
      line.ok_or_else(|| bad(format!("no {key}")))
    };
    let number = |key: &str| -> io::Result<u32> {
      get(key)?.parse().map_err(|_| bad(format!("{key} is not a number")))
    };

    Ok(Self {
      system: get("system")?,
      cpus: number("cpus")?,
      memory: number("memory")?,
      disk: number("disk")?,
    })
  }

  fn text(&self) -> String {
    let Self { system, cpus, memory, disk } = self;
    format!("system = {system}\ncpus = {cpus}\nmemory = {memory}\ndisk = {disk}\n")
  }
}

pub struct Vm {
  pub name: String,
  /// Its settings' file, in trivm's config folder.
  pub conf: PathBuf,
  /// Its disks, platform files and machine state, in trivm's data folder.
  pub dir: PathBuf,
  /// What its runs write (`log`, `pid`, the sockets), in trivm's state folder.
  pub state_dir: PathBuf,
  pub config: Config,
  /// The NIC's address, `mac` in the data folder: the guest's DHCP lease follows it.
  pub mac: String,
  /// Set until the first boot has made the user and the guest answers SSH with trivm's key: the
  /// mark `setup` in the data folder, as it says what the disk holds.
  pub setup: bool,
  /// The host's port forwarded to the guest's SSH, for a VM on QEMU's user network: `port` in the
  /// state folder, chosen at each start.
  pub port: Option<u16>,
}

impl Vm {
  /// The VM of that name in trivm's folders, its machine state not read.
  pub fn at(name: &str, config: Config) -> Self {
    let conf = config_dir().join(format!("{name}.conf"));
    let (dir, state_dir) = (data_dir().join(name), state_dir().join(name));
    let (mac, setup, port) = (String::new(), false, None);
    Self { name: name.into(), conf, dir, state_dir, config, mac, setup, port }
  }

  pub fn open(name: &str) -> io::Result<Self> {
    let none = || bad(format!("no VM named {name}; see trivm list"));
    if !valid(name) {
      return Err(none());
    }

    let mut vm = Self::at(name, Config::default());
    match vm.read() {
      Err(e) if e.kind() == io::ErrorKind::NotFound => Err(none()),
      x => x.map(|_| vm),
    }
  }

  /// The VM's config and machine state from its files.
  fn read(&mut self) -> io::Result<()> {
    let text = fs::read_to_string(&self.conf)?;
    self.config = Config::parse(&text).map_err(|e| bad(format!("{}: {e}", self.conf.display())))?;
    // A VM whose data is gone, as a `delete` cut short leaves it, opens, so `delete` finishes it.
    self.mac = match fs::read_to_string(self.path("mac")) {
      Ok(x) => x.trim().into(),
      Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
      Err(e) => return Err(e),
    };

    self.setup = self.path(SETUP).exists();
    self.port = fs::read_to_string(self.state("port")).ok().and_then(|x| x.trim().parse().ok());
    Ok(())
  }

  pub fn all() -> io::Result<Vec<Self>> {
    let mut names: Vec<String> = match fs::read_dir(config_dir()) {
      Ok(x) => x
        .filter_map(|x| x.ok()?.file_name().into_string().ok())
        .filter_map(|x| x.strip_suffix(".conf").map(str::to_string))
        .collect(),
      Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
      Err(e) => return Err(e),
    };

    names.sort();
    Ok(names.iter().flat_map(|x| Self::open(x)).collect())
  }

  /// Claims the VM's name by making its data folder, which fails if there is one, so of two VMs of
  /// a name made at once one gets it; then its state folder, and its machine state saved. Nothing
  /// opens it until `commit` writes its config.
  pub fn claim(&self) -> io::Result<()> {
    let name = &self.name;
    if !valid(name) {
      return Err(bad(format!("not a VM name: {name}")));
    }

    socket_fits(&self.state_dir)?;
    if self.conf.exists() {
      return Err(taken(name));
    }

    if let Some(x) = self.dir.parent() {
      made(x)?;
    }

    match fs::DirBuilder::new().mode(0o700).create(&self.dir) {
      Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
        if self.conf.exists() {
          return Err(taken(name));
        }

        let dir = self.dir.display();
        let msg = format!("{dir} is in the way: a VM being made, or left with no config");
        return Err(io::Error::new(e.kind(), msg));
      }
      x => x?,
    }

    let res = made(&self.state_dir).and_then(|_| self.save());
    if res.is_err() {
      let _ = self.remove();
    }

    res
  }

  /// Writes the config of a VM `claim` made, which shows it: whole, beside its place, then linked
  /// there, which fails if a config is there.
  pub fn commit(&self) -> io::Result<()> {
    if let Some(x) = self.conf.parent() {
      made(x)?;
    }

    let tmp = self.conf.with_file_name(format!(".{}.conf.{}", self.name, process::id()));
    fs::write(&tmp, self.config.text())?;
    let res = fs::hard_link(&tmp, &self.conf);
    let _ = fs::remove_file(&tmp);
    match res {
      Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(taken(&self.name)),
      x => x,
    }
  }

  /// Saves the machine state: `mac` and the `setup` mark in the data folder, `port` in the state
  /// folder. A file is written beside its place and renamed there: two `start`s may save at once.
  pub fn save(&self) -> io::Result<()> {
    put(&self.path("mac"), &format!("{}\n", self.mac))?;
    match self.setup {
      true => fs::write(self.path(SETUP), "")?,
      false => gone(fs::remove_file(self.path(SETUP)))?,
    }

    match self.port {
      Some(x) => put(&self.state("port"), &format!("{x}\n")),
      None => gone(fs::remove_file(self.state("port"))),
    }
  }

  /// Removes the VM: its data first, its config last, so a removal cut short leaves a VM that
  /// `delete` finishes, never a disk with no config naming it.
  pub fn remove(&self) -> io::Result<()> {
    gone(fs::remove_dir_all(&self.dir))?;
    gone(fs::remove_dir_all(&self.state_dir))?;
    gone(fs::remove_file(&self.conf))
  }

  /// A file in the VM's data folder.
  pub fn path(&self, file: &str) -> PathBuf {
    self.dir.join(file)
  }

  /// A file in the VM's state folder.
  pub fn state(&self, file: &str) -> PathBuf {
    self.state_dir.join(file)
  }

  /// The pid of the VM's process while it runs: the process holds a lock on its `pid` file, so a
  /// file left by a dead one doesn't count, though its pid may be another process's by now.
  pub fn pid(&self) -> Option<i32> {
    pid_of(&fs::File::open(self.state("pid")).ok()?)
  }

  /// Marks the VM as running, with this process's pid, for as long as the file returned is open.
  pub fn lock(&self) -> io::Result<fs::File> {
    let f = self.pid_file()?;
    match f.try_lock() {
      Ok(()) => {}
      Err(fs::TryLockError::WouldBlock) => {
        return Err(bad(format!("{} is in use: running, being made or cloned", self.name)));
      }
      Err(fs::TryLockError::Error(e)) => return Err(e),
    }

    write_pid(&f, process::id())?;
    Ok(f)
  }

  /// Keeps the VM from starting for as long as the file returned is open: a shared lock on `pid`,
  /// which the VM's process takes exclusive, so clones of a VM made at once don't wait for each
  /// other.
  pub fn hold(&self) -> io::Result<fs::File> {
    let f = self.pid_file()?;
    match f.try_lock_shared() {
      Ok(()) => Ok(f),
      Err(fs::TryLockError::WouldBlock) => {
        Err(bad(format!("{} is running; trivm stop {} first", self.name, self.name)))
      }
      Err(fs::TryLockError::Error(e)) => Err(e),
    }
  }

  /// `pid`, its folder made again if a person deleted it, as the spec lets them.
  fn pid_file(&self) -> io::Result<fs::File> {
    made(&self.state_dir)?;
    fs::OpenOptions::new().create(true).write(true).truncate(false).open(self.state("pid"))
  }

  /// The guest's address: from its newest DHCP lease, or the host's own for a VM on QEMU's user
  /// network, whose SSH is forwarded from `port`.
  pub fn ip(&self) -> Option<String> {
    if self.port.is_some() {
      return Some("127.0.0.1".into());
    }

    let mut text = String::new();
    fs::File::open(LEASES).ok()?.read_to_string(&mut text).ok()?;
    lease(&text, &self.mac)
  }
}

/// The mark of a VM whose first boot hasn't finished, in its data folder.
const SETUP: &str = "setup";

fn taken(name: &str) -> io::Error {
  let msg = format!("{name} exists; trivm delete {name} first");
  io::Error::new(io::ErrorKind::AlreadyExists, msg)
}

/// Writes a file through one of this process's own beside it, renamed over it: a reader sees the
/// old or the new.
fn put(path: &Path, text: &str) -> io::Result<()> {
  let tmp = path.with_added_extension(process::id().to_string());
  fs::write(&tmp, text)?;
  fs::rename(tmp, path)
}

/// A removal that found nothing to remove is done.
fn gone(res: io::Result<()>) -> io::Result<()> {
  match res {
    Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
    x => x,
  }
}

/// trivm's own files beside the VMs' folders in data: the SSH key and its lock.
const OWN: &[&str] = &["id_ed25519", "id_ed25519.pub", "id_ed25519.lock"];

/// A VM's name: a folder in trivm's, not a path out of it or one of trivm's own files, in any case
/// (APFS ignores it), nor a flag to the commands trivm runs with it (`_run <vm>`).
pub fn valid(name: &str) -> bool {
  let chars = name.chars().all(|x| x.is_ascii_alphanumeric() || "-_.".contains(x));
  let own = OWN.iter().any(|x| x.eq_ignore_ascii_case(name));
  chars && !name.is_empty() && !name.starts_with(['.', '-']) && !own
}

/// Checks that the VM's `ctl.sock` in its state folder fits a Unix socket's path, which macOS
/// caps at 104 bytes with the NUL: a VM whose doesn't can't start. QEMU's `qmp.sock` and
/// `tpm.sock` are as long.
pub fn socket_fits(dir: &Path) -> io::Result<()> {
  match dir.join("ctl.sock").as_os_str().len() < 104 {
    true => Ok(()),
    false => Err(bad(format!("{}: too long a path for a socket; a shorter name", dir.display()))),
  }
}

/// A random locally administered unicast MAC address.
pub fn random_mac() -> io::Result<String> {
  let mut b = [0u8; 6];
  fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
  b[0] = (b[0] & 0xfe) | 0x02;
  Ok(mac_text(&b))
}

/// The MAC address of `run`'s clone of a VM: the VM's own with the last three bytes XORed with a
/// hash (FNV-1a) of the clone's name, the same each time, so the clone gets the same lease and
/// address each time, and a clone of a clone none of its ancestors'.
pub fn clone_mac(mac: &str, name: &str) -> io::Result<String> {
  let mut b = parse_mac(mac).ok_or_else(|| bad(format!("not a MAC address: {mac}")))?;
  let hash = name.bytes().fold(0x811c9dc5u32, |h, x| (h ^ x as u32).wrapping_mul(0x01000193));
  let [_, h @ ..] = (hash & 0xff_ffff).max(1).to_be_bytes();
  for (x, h) in b[3..].iter_mut().zip(h) {
    *x ^= h;
  }

  Ok(mac_text(&b))
}

fn mac_text(b: &[u8; 6]) -> String {
  b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(":")
}

unsafe extern "C" {
  fn kill(pid: i32, sig: i32) -> i32;
  fn proc_name(pid: i32, buf: *mut u8, size: u32) -> i32;
}

/// The name of the program a process of that pid runs.
pub fn process_name(pid: i32) -> Option<String> {
  let mut b = [0u8; 64];
  // SAFETY: a buffer of the size given.
  let n = unsafe { proc_name(pid, b.as_mut_ptr(), b.len() as u32) };
  (n > 0).then(|| String::from_utf8_lossy(&b[..n as usize]).into_owned())
}

/// Whether the process of that pid is a VM's, trivm's or QEMU, so not one that took a gone VM's
/// pid since.
pub fn is_vm(pid: i32) -> bool {
  process_name(pid).is_some_and(|x| x.starts_with("trivm") || x.starts_with("qemu"))
}

/// A pid over the one in a locked `pid` file in one write, padded and not truncated first: a
/// reader sees one pid or the other, never an empty file or a mix.
pub fn write_pid(f: &fs::File, pid: u32) -> io::Result<()> {
  f.write_all_at(&pid_text(pid), 0)?;
  f.set_len(10)
}

/// A pid as the `pid` file holds it, its digits padded with spaces to 10 bytes; made with no
/// allocation, as a child between fork and exec writes its own.
pub fn pid_text(pid: u32) -> [u8; 10] {
  let mut b = [b' '; 10];
  let (mut n, len) = (pid, pid.checked_ilog10().unwrap_or(0) as usize + 1);
  for x in b[..len].iter_mut().rev() {
    *x = b'0' + (n % 10) as u8;
    n /= 10;
  }

  b
}

/// The pid in an open `pid` file while its process holds the lock on it.
pub fn pid_of(f: &fs::File) -> Option<i32> {
  if !matches!(f.try_lock_shared(), Err(fs::TryLockError::WouldBlock)) {
    return None;
  }

  // The process writes its pid just after taking the lock: until then the file is empty, or has
  // the pid of the one before, gone.
  let mut last = None;
  for _ in 0..50 {
    let mut buf = [0; 16];
    let n = f.read_at(&mut buf, 0).ok()?;
    last = std::str::from_utf8(&buf[..n]).ok().and_then(|x| x.trim().parse().ok()).or(last);
    if let Some(pid) = last
      && alive(pid)
    {
      return Some(pid);
    }

    thread::sleep(Duration::from_millis(10));
  }

  last
}

/// Whether a process of that pid runs: one of another user's can't be signalled (EPERM) but runs.
fn alive(pid: i32) -> bool {
  // SAFETY: signal 0 only checks the pid.
  let res = unsafe { kill(pid, 0) };
  res == 0 || io::Error::last_os_error().raw_os_error() == Some(1)
}

pub fn signal(pid: i32, sig: i32) {
  // SAFETY: a plain syscall on a pid.
  unsafe { kill(pid, sig) };
}

/// Where macOS's bootpd, which VZ's NAT network uses, writes its leases.
const LEASES: &str = "/var/db/dhcpd_leases";

/// The address leased last to `mac` in bootpd's lease file, which writes each byte of the MAC
/// without its leading zero (`hw_address=1,a:b2:3:...`).
fn lease(text: &str, mac: &str) -> Option<String> {
  let want = parse_mac(mac)?;
  let mut best: Option<(u64, String)> = None;
  for block in text.split('}') {
    let field = |key: &str| {
      block.lines().find_map(|x| x.trim().strip_prefix(key)?.strip_prefix('=').map(str::to_string))
    };
    let Some(hw) = field("hw_address") else { continue };
    let Some(ip) = field("ip_address") else { continue };
    let Some(hw) = hw.split_once(',').and_then(|(_, x)| parse_mac(x)) else { continue };
    if hw != want {
      continue;
    }

    let time =
      field("lease").and_then(|x| u64::from_str_radix(x.trim_start_matches("0x"), 16).ok());
    let time = time.unwrap_or(0);
    if best.as_ref().is_none_or(|(t, _)| time >= *t) {
      best = Some((time, ip));
    }
  }

  best.map(|(_, ip)| ip)
}

fn parse_mac(x: &str) -> Option<[u8; 6]> {
  let parts: Vec<u8> =
    x.split(':').map(|x| u8::from_str_radix(x, 16).ok()).collect::<Option<_>>()?;
  parts.try_into().ok()
}

/// The space a sparse file takes on disk, as `12.3G`.
pub fn size(path: &Path) -> String {
  use std::os::unix::fs::MetadataExt;
  let bytes = fs::metadata(path).map(|x| x.blocks() * 512).unwrap_or(0);
  format!("{:.1}G", bytes as f64 / GIB as f64)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn config() {
    let c = Config { system: "macos-27".into(), cpus: 4, memory: 8, disk: 64 };
    assert_eq!(c.text(), "system = macos-27\ncpus = 4\nmemory = 8\ndisk = 64\n");
    assert_eq!(Config::parse(&c.text()).unwrap(), c);
    assert_eq!(c.os(), Os::Macos);
    assert!(c.provisioned());
    assert!(!Config { system: "macos-26".into(), ..c.clone() }.provisioned());
    assert_eq!(c.memory_bytes(), 8 << 30);
    assert!(Config::parse("system = x\n").is_err());
    // A line it doesn't know, such as the machine state the config used to hold, is left.
    let old = Config::parse("system=a\ncpus=1\nmemory=2\ndisk=3\nmac=m\nsetup = true\n");
    assert_eq!(old.unwrap().text(), "system = a\ncpus = 1\nmemory = 2\ndisk = 3\n");
    assert_eq!(Config { system: "windows-11".into(), ..c }.os(), Os::Windows);
  }

  #[test]
  fn claim() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("trivm-claim-{}", process::id()));
    let config = Config { system: "windows-11".into(), cpus: 1, memory: 2, disk: 3 };
    let mac = "02:00:00:00:00:01";
    let at = |name: &str| Vm {
      conf: root.join(format!("config/{name}.conf")),
      dir: root.join("data").join(name),
      state_dir: root.join("state").join(name),
      mac: mac.into(),
      setup: true,
      ..Vm::at(name, config.clone())
    };

    // Claimed: its folders, its owner's only, and its machine state; no config, so not opened.
    let mut vm = at("a");
    vm.claim().unwrap();
    assert_eq!(fs::metadata(&vm.dir).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(fs::read_to_string(vm.path("mac")).unwrap(), format!("{mac}\n"));
    assert!(vm.path("setup").exists() && vm.state_dir.is_dir() && !vm.conf.exists());
    assert_eq!(at("a").read().unwrap_err().kind(), io::ErrorKind::NotFound);
    assert_eq!(at("a").claim().unwrap_err().kind(), io::ErrorKind::AlreadyExists);

    vm.commit().unwrap();
    assert_eq!(fs::read_to_string(&vm.conf).unwrap(), config.text());
    assert_eq!(vm.commit().unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(at("a").claim().unwrap_err().to_string(), "a exists; trivm delete a first");

    // Set up, then started on QEMU's network: read back as saved.
    vm.setup = false;
    vm.port = Some(50022);
    vm.save().unwrap();
    let mut read = Vm { mac: String::new(), ..at("a") };
    read.read().unwrap();
    assert_eq!((&read.config, read.mac.as_str()), (&config, mac));
    assert_eq!((read.setup, read.port), (false, Some(50022)));
    vm.port = None;
    vm.save().unwrap();
    assert!(!vm.state("port").exists());

    // Removed, the name is free; its data folder left with no config is in the way.
    vm.remove().unwrap();
    assert!(!vm.dir.exists() && !vm.state_dir.exists() && !vm.conf.exists());
    made(&vm.dir).unwrap();
    let err = at("a").claim().unwrap_err();
    fs::remove_dir_all(&root).unwrap();
    assert_eq!(
      err.to_string(),
      format!("{} is in the way: a VM being made, or left with no config", vm.dir.display())
    );
  }

  #[test]
  fn leases() {
    let text = "{\n\tname=a\n\tip_address=192.168.64.3\n\thw_address=1,2:a:0:b:c:d\n\tlease=0x10\n}\n\
      {\n\tname=b\n\tip_address=192.168.64.9\n\thw_address=1,2:a:0:b:c:d\n\tlease=0x20\n}\n\
      {\n\tname=c\n\tip_address=192.168.64.4\n\thw_address=1,2:a:0:b:c:e\n\tlease=0x30\n}\n";
    assert_eq!(lease(text, "02:0a:00:0b:0c:0d").as_deref(), Some("192.168.64.9"));
    assert_eq!(lease(text, "02:0a:00:0b:0c:0e").as_deref(), Some("192.168.64.4"));
    assert_eq!(lease(text, "02:0a:00:0b:0c:0f"), None);
  }

  #[test]
  fn names() {
    for x in ["macos-27", "ubuntu-24.04", "a_b"] {
      assert!(valid(x), "{x}");
    }

    // 63 bytes of a name in this Mac's state folder.
    let state = Path::new("/Users/user/.local/state/trivm");
    assert!(socket_fits(&state.join("macos-27-run-1")).is_ok());
    assert!(socket_fits(&state.join("x".repeat(63))).is_ok());
    assert!(socket_fits(&state.join("x".repeat(64))).is_err());

    // Their own files no longer beside the VMs' folders.
    for x in ["cache", "entitlements.plist", "locks"] {
      assert!(valid(x), "{x}");
    }

    let own = ["id_ed25519", "ID_ED25519.pub", "id_ed25519.lock"];
    for x in ["", ".", "..", "../x", "/tmp/x", "a/b", ".a", "-a", "--help"].into_iter().chain(own) {
      assert!(!valid(x), "{x}");
      assert!(Vm::open(x).is_err(), "{x}");
    }
  }

  #[test]
  fn folders() {
    let home = Path::new("/Users/u");
    let var = |x: &str| Some(OsString::from(x));
    assert_eq!(base(var("/x/data"), home, ".local/share"), Path::new("/x/data"));
    // Unset, empty or relative: ignored, as the spec says.
    for x in [None, var(""), var("data"), var("./data")] {
      assert_eq!(base(x, home, ".local/share"), Path::new("/Users/u/.local/share"));
    }
  }

  #[test]
  fn owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("trivm-private-{}", std::process::id()));
    made(&dir.join("a/b")).unwrap();
    made(&dir.join("a/b")).unwrap();
    let mode = |x: &str| fs::metadata(dir.join(x)).unwrap().permissions().mode() & 0o777;
    let modes = [mode(""), mode("a"), mode("a/b")];
    fs::remove_dir_all(&dir).unwrap();
    assert_eq!(modes, [0o700; 3]);
  }

  #[test]
  fn lock() {
    let dir = std::env::temp_dir().join(format!("trivm-lock-{}", std::process::id()));
    let config = Config::parse("system=a\ncpus=1\nmemory=2\ndisk=3\n").unwrap();
    // The state folder made by the lock.
    let (conf, data, state_dir) = (dir.join("a.conf"), dir.join("data"), dir.join("state"));
    let vm = Vm { conf, dir: data, state_dir, ..Vm::at("a", config) };
    assert_eq!(vm.pid(), None);

    let lock = vm.lock().unwrap();
    assert_eq!(vm.pid(), Some(std::process::id() as i32));

    // QEMU's pid over trivm's, shorter or longer.
    write_pid(&lock, 42).unwrap();
    assert_eq!(vm.pid(), Some(42));
    write_pid(&lock, 1234567).unwrap();
    assert_eq!(fs::read_to_string(vm.state("pid")).unwrap(), "1234567   ");
    assert_eq!(&pid_text(0), b"0         ");
    assert_eq!(&pid_text(u32::MAX), b"4294967295");
    // The test's own binary is `trivm-<hash>`; launchd is root's, its name not ours to read.
    assert!(is_vm(std::process::id() as i32));
    assert!(!is_vm(1) && !is_vm(-1));
    write_pid(&lock, std::process::id()).unwrap();

    assert!(vm.lock().is_err());
    assert!(vm.hold().is_err());
    drop(lock);
    // A process another test forks meanwhile shares the lock until it execs: 8 ms here.
    let t = std::time::Instant::now();
    while vm.pid().is_some() && t.elapsed().as_secs() < 1 {
      std::thread::sleep(std::time::Duration::from_millis(5));
    }

    assert_eq!(vm.pid(), None);

    // Two holds at once, which keep the VM from running and don't count as it running.
    let (a, b) = (vm.hold().unwrap(), vm.hold().unwrap());
    assert_eq!(vm.pid(), None);
    assert!(vm.lock().is_err());
    drop((a, b));
    assert!(vm.lock().is_ok());
    fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn mac() {
    let m = random_mac().unwrap();
    let b = parse_mac(&m).unwrap();
    assert_eq!(b[0] & 0x03, 0x02);

    // The same each time, another for each clone, a clone of a clone's not its original's.
    let m = "02:0a:00:0b:0c:0d";
    let (one, two) = (clone_mac(m, "a-run-1").unwrap(), clone_mac(m, "a-run-2").unwrap());
    assert_eq!(one, clone_mac(m, "a-run-1").unwrap());
    assert!(one.starts_with("02:0a:00:") && one != m && one != two);
    let nested = clone_mac(&one, "a-run-1-run-1").unwrap();
    assert!(nested != m && nested != one && nested != two);
    assert!(clone_mac("02:0a", "a").is_err());
  }
}
