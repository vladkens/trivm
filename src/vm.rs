// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! A VM's folder in `~/.trivm`: its config, its files, whether its process runs, and the guest's
//! address from the host's DHCP leases.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::bad;

/// The guest's user and password, the same in every VM.
pub const USER: &str = "user";
pub const PASSWORD: &str = "1234";

const GIB: u64 = 1 << 30;

/// trivm's folder: `TRIVM_HOME`, or `~/.trivm`.
pub fn home() -> PathBuf {
  match std::env::var_os("TRIVM_HOME") {
    Some(x) if !x.is_empty() => PathBuf::from(x),
    _ => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".trivm"),
  }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Os {
  Macos,
  Linux,
}

#[derive(Debug, PartialEq)]
pub struct Config {
  pub system: String,
  pub cpus: u32,
  /// GiB.
  pub memory: u32,
  /// GiB.
  pub disk: u32,
  pub mac: String,
  /// Set until the first boot has made the user and the guest answers SSH with trivm's key.
  pub setup: bool,
}

impl Config {
  pub fn os(&self) -> Os {
    match self.system.starts_with("macos") {
      true => Os::Macos,
      false => Os::Linux,
    }
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
      line.ok_or_else(|| bad(format!("config: no {key}")))
    };
    let number = |key: &str| -> io::Result<u32> {
      get(key)?.parse().map_err(|_| bad(format!("config: {key} is not a number")))
    };

    Ok(Self {
      system: get("system")?,
      cpus: number("cpus")?,
      memory: number("memory")?,
      disk: number("disk")?,
      mac: get("mac")?,
      setup: get("setup").is_ok_and(|x| x == "true"),
    })
  }

  fn text(&self) -> String {
    let Self { system, cpus, memory, disk, mac, setup } = self;
    format!(
      "system = {system}\ncpus = {cpus}\nmemory = {memory}\ndisk = {disk}\nmac = {mac}\nsetup = {setup}\n"
    )
  }
}

pub struct Vm {
  pub name: String,
  pub dir: PathBuf,
  pub config: Config,
}

impl Vm {
  pub fn open(name: &str) -> io::Result<Self> {
    let none = || bad(format!("no VM named {name}; see trivm list"));
    if !valid(name) {
      return Err(none());
    }

    let dir = home().join(name);
    let text = match fs::read_to_string(dir.join("config")) {
      Ok(x) => x,
      Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(none()),
      Err(e) => return Err(e),
    };

    Ok(Self { name: name.into(), dir, config: Config::parse(&text)? })
  }

  /// A new VM's folder with its config; the files of its system come next.
  pub fn new(name: &str, config: Config) -> io::Result<Self> {
    if !valid(name) {
      return Err(bad(format!("not a VM name: {name}")));
    }

    // The config made only if there is none, so of two creates of a name one fails.
    let dir = home().join(name);
    fs::create_dir_all(&dir)?;
    match fs::OpenOptions::new().write(true).create_new(true).open(dir.join("config")) {
      Ok(_) => {}
      Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
        return Err(bad(format!("{name} exists; trivm delete {name} first")));
      }
      Err(e) => return Err(e),
    }

    let vm = Self { name: name.into(), dir, config };
    vm.save()?;
    Ok(vm)
  }

  pub fn all() -> io::Result<Vec<Self>> {
    let mut names: Vec<String> = match fs::read_dir(home()) {
      Ok(x) => x.filter_map(|x| x.ok()?.file_name().into_string().ok()).collect(),
      Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
      Err(e) => return Err(e),
    };

    names.sort();
    Ok(
      names
        .iter()
        .filter(|x| home().join(x).join("config").exists())
        .flat_map(|x| Self::open(x))
        .collect(),
    )
  }

  pub fn save(&self) -> io::Result<()> {
    // A file of this process's own: two `start`s may save at once.
    let tmp = self.dir.join(format!("config.{}", std::process::id()));
    fs::write(&tmp, self.config.text())?;
    fs::rename(tmp, self.dir.join("config"))
  }

  pub fn path(&self, file: &str) -> PathBuf {
    self.dir.join(file)
  }

  /// The pid of the VM's process while it runs: the process holds a lock on its `pid` file, so a
  /// file left by a dead one doesn't count, though its pid may be another process's by now.
  pub fn pid(&self) -> Option<i32> {
    let f = fs::File::open(self.path("pid")).ok()?;
    if !matches!(f.try_lock_shared(), Err(fs::TryLockError::WouldBlock)) {
      return None;
    }

    io::read_to_string(f).ok()?.trim().parse().ok()
  }

  /// Marks the VM as running, with this process's pid, for as long as the file returned is open.
  pub fn lock(&self) -> io::Result<fs::File> {
    let mut f =
      fs::OpenOptions::new().create(true).write(true).truncate(false).open(self.path("pid"))?;
    match f.try_lock() {
      Ok(()) => {}
      Err(fs::TryLockError::WouldBlock) => return Err(bad(format!("{} is running", self.name))),
      Err(fs::TryLockError::Error(e)) => return Err(e),
    }

    f.set_len(0)?;
    write!(f, "{}", std::process::id())?;
    Ok(f)
  }

  /// Keeps the VM from starting for as long as the file returned is open: a shared lock on `pid`,
  /// which the VM's process takes exclusive, so clones of a VM made at once don't wait for each
  /// other.
  pub fn hold(&self) -> io::Result<fs::File> {
    let f =
      fs::OpenOptions::new().create(true).write(true).truncate(false).open(self.path("pid"))?;
    match f.try_lock_shared() {
      Ok(()) => Ok(f),
      Err(fs::TryLockError::WouldBlock) => {
        Err(bad(format!("{} is running; trivm stop {} first", self.name, self.name)))
      }
      Err(fs::TryLockError::Error(e)) => Err(e),
    }
  }

  /// The guest's address from its newest DHCP lease.
  pub fn ip(&self) -> Option<String> {
    let mut text = String::new();
    fs::File::open(LEASES).ok()?.read_to_string(&mut text).ok()?;
    lease(&text, &self.config.mac)
  }
}

/// A VM's name: a folder in trivm's, not a path out of it or the cache.
pub fn valid(name: &str) -> bool {
  let chars = name.chars().all(|x| x.is_ascii_alphanumeric() || "-_.".contains(x));
  chars && !name.is_empty() && !name.starts_with('.') && name != "cache"
}

/// A random locally administered unicast MAC address.
pub fn random_mac() -> io::Result<String> {
  let mut b = [0u8; 6];
  fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
  b[0] = (b[0] & 0xfe) | 0x02;
  Ok(mac_text(&b))
}

/// The MAC address of `run`'s `n`th clone of a VM: the VM's own with the last byte XORed with
/// `n`, the same each time, so the clone gets the same lease and address each time.
pub fn clone_mac(mac: &str, n: u8) -> io::Result<String> {
  let mut b = parse_mac(mac).ok_or_else(|| bad(format!("config: not a MAC address: {mac}")))?;
  b[5] ^= n;
  Ok(mac_text(&b))
}

fn mac_text(b: &[u8; 6]) -> String {
  b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(":")
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
  use std::os::unix::fs::MetadataExt;
  let bytes = fs::metadata(path).map(|x| x.blocks() * 512).unwrap_or(0);
  format!("{:.1}G", bytes as f64 / GIB as f64)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn config() {
    let c = Config {
      system: "macos-27".into(),
      cpus: 4,
      memory: 8,
      disk: 64,
      mac: "02:00:00:00:00:01".into(),
      setup: true,
    };
    assert_eq!(Config::parse(&c.text()).unwrap(), c);
    assert_eq!(c.os(), Os::Macos);
    assert_eq!(c.memory_bytes(), 8 << 30);
    assert!(Config::parse("system = x\n").is_err());
    assert!(!Config::parse("system=a\ncpus=1\nmemory=2\ndisk=3\nmac=m\n").unwrap().setup);
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

    for x in ["", ".", "..", "../x", "/tmp/x", "a/b", ".a", "cache"] {
      assert!(!valid(x), "{x}");
      assert!(Vm::open(x).is_err(), "{x}");
    }
  }

  #[test]
  fn lock() {
    let dir = std::env::temp_dir().join(format!("trivm-lock-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let config = Config::parse("system=a\ncpus=1\nmemory=2\ndisk=3\nmac=m\n").unwrap();
    let vm = Vm { name: "a".into(), dir: dir.clone(), config };
    assert_eq!(vm.pid(), None);

    let lock = vm.lock().unwrap();
    assert_eq!(vm.pid(), Some(std::process::id() as i32));
    assert!(vm.lock().is_err());
    assert!(vm.hold().is_err());
    drop(lock);
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

    assert_eq!(clone_mac("02:0a:00:0b:0c:0d", 1).unwrap(), "02:0a:00:0b:0c:0c");
    assert_eq!(clone_mac("02:0a:00:0b:0c:0d", 2).unwrap(), "02:0a:00:0b:0c:0f");
    assert!(clone_mac("02:0a", 1).is_err());
  }
}
