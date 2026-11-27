// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! trivm's folders, by the XDG Base Directory spec, and a VM's files in them: its config, its
//! disks, what its runs write; whether its process runs, and the guest's address from the host's
//! DHCP leases.

use std::ffi::OsString;
use std::io::{self, Read, Seek, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::{env, fmt, fs, process};

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
  /// `screen`, set by the person; else the system's.
  pub screen: Option<Screen>,
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
  /// for macOS 27 and later; an older macOS gets its user from files on its Data volume.
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

  pub fn screen(&self) -> Screen {
    self.screen.unwrap_or(Screen::of(self.os()))
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

    let mut c = Self {
      system: get("system")?,
      cpus: number("cpus")?,
      memory: number("memory")?,
      disk: number("disk")?,
      screen: None,
    };
    c.screen = get("screen").ok().map(|x| Screen::parse(&x, c.os())).transpose()?;
    Ok(c)
  }

  fn text(&self) -> String {
    let Self { system, cpus, memory, disk, screen } = self;
    let text = format!("system = {system}\ncpus = {cpus}\nmemory = {memory}\ndisk = {disk}\n");
    match screen {
      Some(x) => format!("{text}screen = {x}\n"),
      None => text,
    }
  }
}

/// A guest's screen: its size in points, a shot's pixels, and the display's pixels per point.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Screen {
  pub width: f64,
  pub height: f64,
  pub scale: f64,
}

impl Screen {
  /// The system's: on macOS twice 1600×900 points at a Retina density, which it draws HiDPI.
  pub fn of(os: Os) -> Self {
    match os {
      Os::Macos => Self { width: 1600.0, height: 900.0, scale: 2.0 },
      Os::Linux | Os::Windows => Self { width: 1920.0, height: 1080.0, scale: 1.0 },
    }
  }

  /// `<w>x<h>[@<scale>]`, the scale the system's when left out: macOS draws at 1x or 2x, Linux's
  /// desktops at 1x here, as VZ gives them no density and each scales by its own settings.
  pub fn parse(x: &str, os: Os) -> io::Result<Self> {
    let wrong = || bad(format!("not a screen size: {x}, e.g. 1152x720@2"));
    let num = |x: &str| x.parse::<u32>().map_err(|_| wrong());
    let (size, scale) = x.split_once('@').map_or((x, None), |(a, b)| (a, Some(b)));
    let (w, h) = size.split_once('x').ok_or_else(wrong)?;
    let (w, h) = (num(w)?, num(h)?);
    let scale = scale.map(num).transpose()?.unwrap_or(Self::of(os).scale as u32);

    match (os, scale) {
      (Os::Macos, 1 | 2) | (Os::Linux | Os::Windows, 1) => {}
      (Os::Macos, _) => return Err(bad(format!("{x}: macOS draws at 1x or 2x"))),
      _ => return Err(bad(format!("{x}: Linux draws at 1x here; its desktop has its own scale"))),
    }

    // A mode of Linux's virtio-gpu is CVT's, a multiple of 8 pixels wide: 1366 draws 1360.
    if os != Os::Macos && w % 8 != 0 {
      return Err(bad(format!("{x}: Linux's width is a multiple of 8, as {}x{h}", w / 8 * 8)));
    }

    if w < 640 || h < 480 || w > 7680 / scale || h > 4320 / scale {
      return Err(bad(format!("{x}: from 640x480 points up to 7680x4320 pixels")));
    }

    Ok(Self { width: w as f64, height: h as f64, scale: scale as f64 })
  }
}

impl fmt::Display for Screen {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    write!(f, "{}x{}@{}", self.width, self.height, self.scale)
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
  /// Whether Windows has virtio-win's display driver and draws on virtio-gpu: the mark `gpu` in
  /// the data folder, which `create` writes; a Windows VM made before has neither, and ramfb.
  pub gpu: bool,
}

impl Vm {
  /// The VM of that name in trivm's folders, its machine state not read.
  pub fn at(name: &str, config: Config) -> Self {
    let conf = config_dir().join(format!("{name}.conf"));
    let (dir, state_dir) = (data_dir().join(name), state_dir().join(name));
    let (mac, setup, port, gpu) = (String::new(), false, None, false);
    Self { name: name.into(), conf, dir, state_dir, config, mac, setup, port, gpu }
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
    self.gpu = self.path(GPU).exists();
    self.port = self.saved_port();
    Ok(())
  }

  /// The forwarded SSH port its last start saved, read now: a running VM's current one.
  pub fn saved_port(&self) -> Option<u16> {
    fs::read_to_string(self.state("port")).ok().and_then(|x| x.trim().parse().ok())
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

  /// Claims the VM's name by making its data folder, which fails if there is one: of two VMs of a
  /// name made at once one gets it. Its state follows, held while the file returned is open, and
  /// nothing opens it until `commit` writes its config.
  pub fn claim(&self) -> io::Result<fs::File> {
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

    let res = made(&self.state_dir).and_then(|_| self.save()).and_then(|_| self.hold());
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

  /// Saves the machine state: `mac` and the `setup` mark in the data folder. A file is written
  /// beside its place and renamed there: two `start`s may save at once.
  pub fn save(&self) -> io::Result<()> {
    put(&self.path("mac"), &format!("{}\n", self.mac))?;
    match self.setup {
      true => fs::write(self.path(SETUP), ""),
      false => gone(fs::remove_file(self.path(SETUP))),
    }
  }

  /// Saves `port` in the state folder, only by the start holding the VM: a start that saved one
  /// earlier may have another by now.
  pub fn save_port(&self) -> io::Result<()> {
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
    let mut f = self.take()?;
    write!(f, "{}", process::id())?;
    Ok(f)
  }

  /// Keeps the VM to this process for as long as the file returned is open, as `delete` does: the
  /// lock on `pid`, emptied, so no other process takes this one for the VM's and kills it.
  pub fn take(&self) -> io::Result<fs::File> {
    let f = self.pid_file()?;
    match f.try_lock() {
      Ok(()) => {}
      Err(fs::TryLockError::WouldBlock) => {
        return Err(bad(format!(
          "{} is in use: running, being made, cloned or deleted",
          self.name
        )));
      }
      Err(fs::TryLockError::Error(e)) => return Err(e),
    }

    self.current(&f)?;
    f.set_len(0)?;
    Ok(f)
  }

  /// Keeps the VM from starting or being deleted for as long as the file returned is open: a
  /// shared lock on `pid`, which the VM's process takes exclusive, so clones of a VM made at once
  /// don't wait for each other.
  pub fn hold(&self) -> io::Result<fs::File> {
    let f = self.pid_file()?;
    match f.try_lock_shared() {
      Ok(()) => {}
      Err(fs::TryLockError::WouldBlock) => {
        return Err(bad(format!("{} is running; trivm stop {} first", self.name, self.name)));
      }
      Err(fs::TryLockError::Error(e)) => return Err(e),
    }

    self.current(&f)?;
    Ok(f)
  }

  /// Holds the VM's setup for as long as the file returned is open, waiting while another process
  /// sets it up, or failing unless `wait`: a lock on its `setup` mark; none for a VM set up.
  pub fn setting_up(&self, wait: bool) -> io::Result<Option<fs::File>> {
    let f = match fs::File::open(self.path(SETUP)) {
      Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
      x => x?,
    };

    match f.try_lock() {
      Ok(()) => {}
      Err(fs::TryLockError::WouldBlock) if wait => {
        eprintln!("waiting for another start setting {} up", self.name);
        f.lock()?;
      }
      Err(fs::TryLockError::WouldBlock) => {
        return Err(bad(format!("{} is in use: a start sets it up", self.name)));
      }
      Err(fs::TryLockError::Error(e)) => return Err(e),
    }

    Ok(Some(f))
  }

  /// Whether `lock`, from `setting_up`, is on the VM's own `setup` mark, or none on a VM set up: a
  /// VM deleted meanwhile and made again of the name has a mark of its own to wait on.
  pub fn holds(&self, lock: &Option<fs::File>) -> io::Result<bool> {
    let mark = match fs::metadata(self.path(SETUP)) {
      Err(e) if e.kind() == io::ErrorKind::NotFound => None,
      x => Some(x?.ino()),
    };

    let held = lock.as_ref().map(|f| f.metadata().map(|x| x.ino())).transpose()?;
    Ok(mark == held)
  }

  /// Checks that the `pid` this process locked is still the VM's: a `delete` removes it, maybe
  /// while another has it open, and a VM made again of the name has one of its own.
  fn current(&self, f: &fs::File) -> io::Result<()> {
    let ino = fs::metadata(self.state("pid")).map(|x| x.ino()).ok();
    match ino == Some(f.metadata()?.ino()) {
      true => Ok(()),
      false => Err(bad(format!("{} was deleted meanwhile; see trivm list", self.name))),
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

/// The mark of a Windows VM whose disk has virtio-gpu's driver, in its data folder.
pub const GPU: &str = "gpu";

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
pub fn gone(res: io::Result<()>) -> io::Result<()> {
  match res {
    Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
    x => x,
  }
}

/// trivm's own files beside the VMs' folders in data: the SSH key and its lock.
const OWN: &[&str] = &["id_ed25519", "id_ed25519.pub", "id_ed25519.lock"];

/// A VM's name: a folder in trivm's, not a path out of it or one of trivm's own files, in any case
/// (APFS ignores it), nor a flag to the commands trivm runs with it (`_run <vm>`).
fn valid(name: &str) -> bool {
  let chars = name.chars().all(|x| x.is_ascii_alphanumeric() || "-_.".contains(x));
  let own = OWN.iter().any(|x| x.eq_ignore_ascii_case(name));
  chars && !name.is_empty() && !name.starts_with(['.', '-']) && !own
}

/// Checks that the VM's `ctl.sock` in its state folder fits a Unix socket's path, which macOS caps
/// at 104 bytes with the NUL: a VM whose doesn't can't start.
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

/// The pid in an open `pid` file while its process holds the lock on it, read from its start each
/// time, so it can be asked again.
pub fn pid_of(mut f: &fs::File) -> Option<i32> {
  match f.try_lock_shared() {
    Err(fs::TryLockError::WouldBlock) => {}
    // No holder: let go, as the file stays open and would keep others from taking it.
    Ok(()) => {
      let _ = f.unlock();
      return None;
    }
    Err(_) => return None,
  }

  f.seek(io::SeekFrom::Start(0)).ok()?;
  io::read_to_string(f).ok()?.trim().parse().ok()
}

unsafe extern "C" {
  fn kill(pid: i32, sig: i32) -> i32;
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
  let bytes = fs::metadata(path).map(|x| x.blocks() * 512).unwrap_or(0);
  format!("{:.1}G", bytes as f64 / GIB as f64)
}

#[cfg(test)]
pub mod tests {
  use super::*;

  /// Tried again for up to a second: a process another test starts meanwhile shares the locks
  /// open in this one until it execs, 8 ms here.
  pub fn soon<T>(f: impl Fn() -> io::Result<T>) -> T {
    let t = std::time::Instant::now();
    loop {
      match f() {
        Ok(x) => return x,
        Err(e) if t.elapsed().as_secs() >= 1 => panic!("{e}"),
        Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
      }
    }
  }

  /// No one runs the VM, once a process sharing its lock has exec'd.
  fn stopped(vm: &Vm) -> bool {
    soon(|| vm.pid().map_or(Ok(()), |x| Err(bad(format!("{x} runs it")))));
    true
  }

  #[test]
  fn config() {
    let c = Config { system: "macos-27".into(), cpus: 4, memory: 8, disk: 64, screen: None };
    assert_eq!(c.text(), "system = macos-27\ncpus = 4\nmemory = 8\ndisk = 64\n");
    assert_eq!(Config::parse(&c.text()).unwrap(), c);
    assert_eq!(c.os(), Os::Macos);
    assert_eq!(c.memory_bytes(), 8 << 30);
    assert_eq!(c.screen(), Screen { width: 1600.0, height: 900.0, scale: 2.0 });

    // A screen set: written and read back; on Linux at 1x when left out.
    let retina = Screen { width: 1152.0, height: 720.0, scale: 2.0 };
    let set = Config { screen: Some(retina), ..c.clone() };
    assert!(set.text().ends_with("disk = 64\nscreen = 1152x720@2\n"));
    assert_eq!(Config::parse(&set.text()).unwrap(), set);
    let linux = Config::parse("system = debian-13\ncpus=1\nmemory=2\ndisk=3\nscreen = 1280x800\n");
    assert_eq!(linux.unwrap().screen().to_string(), "1280x800@1");
    assert!(
      Config::parse("system = debian-13\ncpus=1\nmemory=2\ndisk=3\nscreen = 1280\n").is_err()
    );

    // Made by VZ's provisioning from 27 on.
    assert!(c.provisioned());
    assert!(!Config { system: "macos-26".into(), ..c.clone() }.provisioned());
    assert!(!Config { system: "ubuntu-22.04".into(), ..c.clone() }.provisioned());

    assert!(Config::parse("system = x\n").is_err());
    // A line it doesn't know, such as the machine state the config used to hold, is left.
    let old = Config::parse("system=a\ncpus=1\nmemory=2\ndisk=3\nmac=m\nsetup = true\n");
    assert_eq!(old.unwrap().text(), "system = a\ncpus = 1\nmemory = 2\ndisk = 3\n");
    assert_eq!(Config { system: "ubuntu-24.04".into(), ..c.clone() }.os(), Os::Linux);
    assert_eq!(Config { system: "windows-11".into(), ..c }.os(), Os::Windows);
  }

  #[test]
  fn screens() {
    let at = |x: &str, os: Os| Screen::parse(x, os).map(|x| x.to_string());
    for (x, os, want) in [
      ("1152x720@2", Os::Macos, "1152x720@2"),
      ("1152x720", Os::Macos, "1152x720@2"),
      ("1440x900@1", Os::Macos, "1440x900@1"),
      ("1280x800", Os::Linux, "1280x800@1"),
      ("640x480@1", Os::Linux, "640x480@1"),
      ("3840x2160@2", Os::Macos, "3840x2160@2"),
      ("1470x956", Os::Macos, "1470x956@2"),
    ] {
      assert_eq!(at(x, os).unwrap(), want);
    }

    for (x, os) in [
      ("1152x720@3", Os::Macos),
      ("1280x800@2", Os::Linux),
      ("1152", Os::Macos),
      ("x720", Os::Macos),
      ("1152x720@", Os::Macos),
      ("1152 x 720", Os::Macos),
      ("-1x720", Os::Macos),
      ("639x480", Os::Linux),
      ("3841x2160@2", Os::Macos),
      ("7680x4321", Os::Linux),
      ("2147483648x480@2", Os::Macos),
      ("4294967295x480", Os::Linux),
      ("1366x768", Os::Linux),
      ("1281x800", Os::Windows),
    ] {
      assert!(at(x, os).is_err(), "{x}");
    }

    assert_eq!(
      at("1280x800@2", Os::Linux).unwrap_err().to_string(),
      "1280x800@2: Linux draws at 1x here; its desktop has its own scale"
    );
    let odd = at("1366x768", Os::Linux).unwrap_err().to_string();
    assert_eq!(odd, "1366x768: Linux's width is a multiple of 8, as 1360x768");
  }

  #[test]
  fn claim() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("trivm-claim-{}", process::id()));
    let config =
      Config { system: "ubuntu-24.04".into(), cpus: 1, memory: 2, disk: 3, screen: None };
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
    let hold = vm.claim().unwrap();
    assert_eq!(fs::metadata(&vm.dir).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(fs::read_to_string(vm.path("mac")).unwrap(), format!("{mac}\n"));
    assert!(vm.path("setup").exists() && vm.state_dir.is_dir() && !vm.conf.exists());
    assert!(vm.lock().is_err());
    assert_eq!(at("a").read().unwrap_err().kind(), io::ErrorKind::NotFound);
    assert_eq!(at("a").claim().unwrap_err().kind(), io::ErrorKind::AlreadyExists);

    vm.commit().unwrap();
    assert_eq!(fs::read_to_string(&vm.conf).unwrap(), config.text());
    assert_eq!(vm.commit().unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(at("a").claim().unwrap_err().to_string(), "a exists; trivm delete a first");

    // Let go and set up: read back as saved.
    drop(hold);
    soon(|| vm.lock());
    vm.setup = false;
    vm.port = Some(50022);
    vm.save().unwrap();
    vm.save_port().unwrap();
    assert_eq!(fs::read_to_string(vm.state("port")).unwrap(), "50022\n");
    let mut read = Vm { mac: String::new(), ..at("a") };
    read.read().unwrap();
    assert_eq!((&read.config, read.mac.as_str(), read.setup), (&config, mac, false));
    assert_eq!((read.port, read.ip().as_deref()), (Some(50022), Some("127.0.0.1")));
    assert!(!read.gpu);

    // A Windows VM with virtio-gpu's driver, as `create` marks one.
    fs::write(vm.path(GPU), "").unwrap();
    read.read().unwrap();
    assert!(read.gpu);

    // Set up by one process at a time; nothing to wait for once it is.
    vm.setup = true;
    vm.save().unwrap();
    let first = vm.setting_up(true).unwrap();
    let second = std::thread::spawn({
      let vm = Vm { mac: String::new(), ..at("a") };
      move || vm.setting_up(true).unwrap().is_some()
    });
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(!second.is_finished());
    assert!(vm.holds(&first).unwrap() && !vm.holds(&None).unwrap());
    let err = vm.setting_up(false).unwrap_err();
    assert_eq!(err.to_string(), "a is in use: a start sets it up");
    drop(first);
    assert!(second.join().unwrap());

    // A lock on the mark of a VM deleted meanwhile isn't the new one's; none on a VM set up is.
    let stale = vm.setting_up(true).unwrap();
    fs::remove_file(vm.path(SETUP)).unwrap();
    vm.save().unwrap();
    assert!(!vm.holds(&stale).unwrap());
    vm.setup = false;
    vm.save().unwrap();
    assert!(vm.setting_up(true).unwrap().is_none() && vm.holds(&None).unwrap());

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
    // `cache` and `entitlements.plist` no longer beside the VMs' folders.
    for x in ["macos-27", "ubuntu-24.04", "a_b", "cache", "entitlements.plist"] {
      assert!(valid(x), "{x}");
    }

    // 63 bytes of a name in this Mac's state folder.
    let state = Path::new("/Users/user/.local/state/trivm");
    assert!(socket_fits(&state.join("macos-27-run-1")).is_ok());
    assert!(socket_fits(&state.join("x".repeat(63))).is_ok());
    assert!(socket_fits(&state.join("x".repeat(64))).is_err());

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
    let dir = std::env::temp_dir().join(format!("trivm-private-{}", process::id()));
    made(&dir.join("a/b")).unwrap();
    made(&dir.join("a/b")).unwrap();
    let mode = |x: &str| fs::metadata(dir.join(x)).unwrap().permissions().mode() & 0o777;
    let modes = [mode(""), mode("a"), mode("a/b")];
    fs::remove_dir_all(&dir).unwrap();
    assert_eq!(modes, [0o700; 3]);
  }

  #[test]
  fn lock() {
    let dir = std::env::temp_dir().join(format!("trivm-lock-{}", process::id()));
    let config = Config::parse("system=a\ncpus=1\nmemory=2\ndisk=3\n").unwrap();
    // The state folder made by the lock.
    let (conf, data, state_dir) = (dir.join("a.conf"), dir.join("data"), dir.join("state"));
    let vm = Vm { conf, dir: data, state_dir, ..Vm::at("a", config) };
    assert_eq!(vm.pid(), None);

    let lock = vm.lock().unwrap();
    assert_eq!(vm.pid(), Some(process::id() as i32));
    assert!(vm.lock().is_err());
    assert!(vm.hold().is_err());
    drop(lock);
    assert!(stopped(&vm));

    // Taken, as by a `delete`: no pid to stop, and nothing else locks it.
    let take = soon(|| vm.take());
    assert_eq!(vm.pid(), None);
    assert!(vm.take().is_err() && vm.lock().is_err() && vm.hold().is_err());
    drop(take);

    // Two holds at once, which keep the VM from running and don't count as it running.
    let (a, b) = (soon(|| vm.hold()), soon(|| vm.hold()));
    assert_eq!(vm.pid(), None);
    assert!(vm.lock().is_err());
    drop((a, b));
    soon(|| vm.lock());

    // A `pid` removed while open, as by a `delete`, and one made again in its place aren't it.
    let old = vm.pid_file().unwrap();
    assert!(vm.current(&old).is_ok());
    fs::remove_dir_all(&vm.state_dir).unwrap();
    assert!(vm.current(&old).is_err());
    let new = vm.pid_file().unwrap();
    assert!(vm.current(&old).is_err() && vm.current(&new).is_ok());
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
