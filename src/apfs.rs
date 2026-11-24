// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! A file's owner set to root in an APFS volume of a disk image, by its inode on disk: a user
//! mounts a volume only with ownership ignored, which stores new files as 99:99, and launchd runs
//! a LaunchDaemon only when root owns it.
//!
//! Lima's `pkg/apfs` (Apache-2.0, https://github.com/lima-vm/lima/blob/master/pkg/apfs/chown.go)
//! in Rust; the layouts are those of Apple's File System Reference. Only an inode's owner and
//! group change, in place, with the node's checksum: nothing is allocated or moved.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;

use crate::bad;

/// A volume's role: the Data volume of a macOS install.
pub const DATA: u16 = 0x40;

const NXSB: u32 = u32::from_le_bytes(*b"NXSB");
const APSB: u32 = u32::from_le_bytes(*b"APSB");
/// The GPT type of an APFS container, as stored: 7C3457EF-0000-11AA-AA11-00306543ECAC.
const APFS_PART: [u8; 16] =
  [0xef, 0x57, 0x34, 0x7c, 0x00, 0x00, 0xaa, 0x11, 0xaa, 0x11, 0x00, 0x30, 0x65, 0x43, 0xec, 0xac];

/// What a volume mounted with ownership ignored stores as a new file's owner and group.
const UNKNOWN: u32 = 99;
const ROOT_DIR: u64 = 2;
const TYPE_INODE: u64 = 3;
const TYPE_DIR_REC: u64 = 9;
const ID_MASK: u64 = (1 << 60) - 1;
const UNENCRYPTED: u64 = 1;
const CASE_INSENSITIVE: u64 = 1;
const NORMALIZATION_INSENSITIVE: u64 = 8;

fn le16(b: &[u8], at: usize) -> u16 {
  u16::from_le_bytes(b[at..at + 2].try_into().unwrap_or_default())
}

fn le32(b: &[u8], at: usize) -> u32 {
  u32::from_le_bytes(b[at..at + 4].try_into().unwrap_or_default())
}

fn le64(b: &[u8], at: usize) -> u64 {
  u64::from_le_bytes(b[at..at + 8].try_into().unwrap_or_default())
}

/// The Fletcher-64 checksum of an object, over all of it after the checksum itself.
fn fletcher(b: &[u8]) -> u64 {
  let m = 0xffff_ffffu64;
  let (mut s1, mut s2) = (0u64, 0u64);
  for x in b[8..].as_chunks::<4>().0 {
    s1 = (s1 + u32::from_le_bytes(*x) as u64) % m;
    s2 = (s2 + s1) % m;
  }

  let lo = m - (s1 + s2) % m;
  let hi = m - (s1 + lo) % m;
  hi << 32 | lo
}

fn valid(b: &[u8]) -> bool {
  le64(b, 0) == fletcher(b)
}

/// A volume's place in the container.
struct Volume {
  /// The physical address of the root of the volume's object map.
  omap: u64,
  /// The virtual address of the root of its file system tree.
  root: u64,
  xid: u64,
  uuid: [u8; 16],
}

struct Container {
  f: File,
  /// Where the container starts in the file: a partition of a GPT disk, or 0.
  base: u64,
  block: usize,
}

impl Container {
  fn open(path: &Path, write: bool) -> io::Result<Self> {
    let f = OpenOptions::new().read(true).write(write).open(path)?;
    let mut head = vec![0u8; 4096];
    f.read_exact_at(&mut head, 0)?;
    let base = match le32(&head, 32) == NXSB {
      true => 0,
      false => partition(&f)?,
    };

    f.read_exact_at(&mut head, base)?;
    let block = le32(&head, 36) as usize;
    if le32(&head, 32) != NXSB || !(4096..=65536).contains(&block) {
      return Err(bad(format!("{}: no APFS container", path.display())));
    }

    Ok(Self { f, base, block })
  }

  fn read(&self, addr: u64) -> io::Result<Vec<u8>> {
    let mut b = vec![0u8; self.block];
    self.f.read_exact_at(&mut b, self.base + addr * self.block as u64)?;
    match valid(&b) {
      true => Ok(b),
      false => Err(bad(format!("APFS: block {addr} has a bad checksum"))),
    }
  }

  /// The container's superblock of the newest checkpoint.
  fn superblock(&self) -> io::Result<Vec<u8>> {
    let zero = self.read(0)?;
    let (base, count) = (le64(&zero, 112), le32(&zero, 104) & 0x7fff_ffff);
    let mut best: Option<Vec<u8>> = None;
    for i in 0..count as u64 {
      let Ok(b) = self.read(base + i) else { continue };
      if le32(&b, 24) & 0xffff != 1 || le32(&b, 32) != NXSB {
        continue;
      }

      if best.as_ref().is_none_or(|x| le64(&b, 16) > le64(x, 16)) {
        best = Some(b);
      }
    }

    best.ok_or_else(|| bad("APFS: no valid checkpoint"))
  }

  /// The physical address of a virtual object, as of the transaction `xid`.
  fn lookup(&self, omap: u64, oid: u64, xid: u64) -> io::Result<u64> {
    let mut b = self.read(omap)?;
    loop {
      let n = Node::new(&b, self.block)?;
      if n.leaf {
        let mut best: Option<(u64, u64)> = None;
        for i in 0..n.count {
          let (k, v) = n.entry(i);
          let key = n.key(k, 16)?;
          let (o, x) = (le64(&b, key), le64(&b, key + 8));
          if o == oid && x <= xid && best.is_none_or(|(bx, _)| x > bx) {
            best = Some((x, le64(&b, n.value(v, 16)? + 8)));
          }
        }

        return best.map(|x| x.1).ok_or_else(|| bad(format!("APFS: object {oid} not mapped")));
      }

      let mut child = 0;
      for i in 0..n.count {
        let key = n.key(n.entry(i).0, 16)?;
        match (le64(&b, key), le64(&b, key + 8)) <= (oid, xid) {
          true => child = i,
          false => break,
        }
      }

      let addr = le64(&b, n.value(n.entry(child).1, 8)?);
      b = self.read(addr)?;
    }
  }

  fn volume(&self, role: u16) -> io::Result<Volume> {
    let sb = self.superblock()?;
    let omap = le64(&self.read(le64(&sb, 160))?, 48);
    let xid = le64(&sb, 16);
    for i in 0..100 {
      let oid = le64(&sb, 184 + i * 8);
      if oid == 0 {
        continue;
      }

      let Ok(addr) = self.lookup(omap, oid, xid) else { continue };
      let Ok(v) = self.read(addr) else { continue };
      if le32(&v, 32) != APSB || le16(&v, 964) != role {
        continue;
      }

      if le64(&v, 264) & UNENCRYPTED == 0 {
        return Err(bad("APFS: the volume is encrypted"));
      }

      // Directory records are keyed by a hash of the name only on volumes that ignore case or
      // normalization, as macOS's are.
      if le64(&v, 56) & (CASE_INSENSITIVE | NORMALIZATION_INSENSITIVE) == 0 {
        return Err(bad("APFS: the volume is case-sensitive"));
      }

      return Ok(Volume {
        omap: le64(&self.read(le64(&v, 128))?, 48),
        root: le64(&v, 136),
        xid: le64(&v, 16),
        uuid: v[240..256].try_into().unwrap_or_default(),
      });
    }

    Err(bad(format!("APFS: no volume with the role {role:#x}")))
  }

  /// Walks the volume's file system tree down to the leaf where `key` would be, comparing keys
  /// by `cmp`; returns the leaf's address and contents.
  fn leaf(
    &self,
    vol: &Volume,
    cmp: impl Fn(&Node, usize) -> io::Result<std::cmp::Ordering>,
  ) -> io::Result<(u64, Vec<u8>)> {
    let mut addr = self.lookup(vol.omap, vol.root, vol.xid)?;
    let mut b = self.read(addr)?;
    loop {
      let n = Node::new(&b, self.block)?;
      if n.leaf {
        return Ok((addr, b));
      }

      let mut child = 0;
      for i in 0..n.count {
        match cmp(&n, n.key(n.entry(i).0, 8)?)?.is_le() {
          true => child = i,
          false => break,
        }
      }

      let oid = le64(&b, n.value(n.entry(child).1, 8)?);
      addr = self.lookup(vol.omap, oid, vol.xid)?;
      b = self.read(addr)?;
    }
  }

  /// The inode of the file `name` in the directory `parent`.
  fn child(&self, vol: &Volume, parent: u64, name: &str) -> io::Result<u64> {
    let want = TYPE_DIR_REC << 60 | parent;
    let hash = name_hash(name);
    let cmp = |n: &Node, key: usize| -> io::Result<std::cmp::Ordering> {
      let head = le64(n.b, key);
      let order = fs_order(head).cmp(&fs_order(want));
      if order.is_ne() {
        return Ok(order);
      }

      let (h, entry) = n.name(key + 8)?;
      Ok((h, entry.to_lowercase()).cmp(&(hash, name.to_lowercase())))
    };

    let (_, b) = self.leaf(vol, cmp)?;
    let n = Node::new(&b, self.block)?;
    for i in 0..n.count {
      let (k, v) = n.entry(i);
      let key = n.key(k, 12)?;
      if le64(&b, key) == want && n.name(key + 8)?.1 == name {
        return Ok(le64(&b, n.value(v, 8)?) & ID_MASK);
      }
    }

    Err(bad(format!("APFS: no {name} in directory {parent}")))
  }

  /// Sets the owner and group of the inode `id` to root, from the 99:99 of a mount that ignored
  /// ownership.
  fn chown(&self, vol: &Volume, id: u64) -> io::Result<()> {
    let want = TYPE_INODE << 60 | id;
    let cmp = |n: &Node, key: usize| Ok(fs_order(le64(n.b, key)).cmp(&fs_order(want)));
    let (addr, mut b) = self.leaf(vol, cmp)?;
    let n = Node::new(&b, self.block)?;
    let mut at = None;
    for i in 0..n.count {
      let (k, v) = n.entry(i);
      if le64(&b, n.key(k, 8)?) == want {
        at = Some(n.value(v, 80)?);
        break;
      }
    }

    let at = at.ok_or_else(|| bad(format!("APFS: no inode {id}")))?;
    if le64(&b, at + 8) != id {
      return Err(bad(format!("APFS: inode {id} isn't its own")));
    }

    match (le32(&b, at + 72), le32(&b, at + 76)) {
      (0, 0) => return Ok(()),
      (UNKNOWN, UNKNOWN) => {}
      (u, g) => return Err(bad(format!("APFS: inode {id} is {u}:{g}, not {UNKNOWN}:{UNKNOWN}"))),
    }

    b[at + 72..at + 80].fill(0);
    let sum = fletcher(&b);
    b[..8].copy_from_slice(&sum.to_le_bytes());
    self.f.write_all_at(&b, self.base + addr * self.block as u64)
  }
}

/// The first APFS container of a GPT disk: its offset in bytes.
fn partition(f: &File) -> io::Result<u64> {
  let mut head = [0u8; 512];
  f.read_exact_at(&mut head, 512)?;
  if &head[..8] != b"EFI PART" {
    return Err(bad("no GPT and no APFS container"));
  }

  let (lba, count, size) = (le64(&head, 72), le32(&head, 80), le32(&head, 84) as usize);
  if !(128..=4096).contains(&size) || count as usize * size > 4 << 20 {
    return Err(bad("GPT: its partition table is out of range"));
  }

  let mut table = vec![0u8; count as usize * size];
  f.read_exact_at(&mut table, lba * 512)?;
  for x in table.chunks(size) {
    if x[..16] == APFS_PART {
      return Ok(le64(x, 32) * 512);
    }

    if x[..16].iter().all(|x| *x == 0) {
      break;
    }
  }

  Err(bad("GPT: no APFS container"))
}

/// A file system key's order: by object, then by record type.
fn fs_order(head: u64) -> (u64, u64) {
  (head & ID_MASK, head >> 60)
}

/// The hash in a directory record's key: CRC-32C of the name as UTF-32LE, not inverted at the
/// end, 22 bits. The volume ignores case, so the name is folded first, to lower case as Lima does
/// (exact for ASCII; Apple's folding and normalization aren't done here).
fn name_hash(name: &str) -> u32 {
  let mut crc = !0u32;
  for b in name.to_lowercase().chars().flat_map(|x| (x as u32).to_le_bytes()) {
    crc ^= b as u32;
    for _ in 0..8 {
      crc = if crc & 1 != 0 { crc >> 1 ^ 0x82f6_3b78 } else { crc >> 1 };
    }
  }

  crc & 0x3f_ffff
}

/// A node of a B-tree: a table of entries, keys growing up after it, values down from the end
/// (before the tree's info in a root node). Offsets from the block are checked to stay in it.
struct Node<'a> {
  b: &'a [u8],
  count: usize,
  toc: usize,
  keys: usize,
  end: usize,
  leaf: bool,
  fixed: bool,
}

impl<'a> Node<'a> {
  fn new(b: &'a [u8], block: usize) -> io::Result<Self> {
    let kind = le32(b, 24) & 0xffff;
    if kind != 2 && kind != 3 {
      return Err(bad(format!("APFS: not a B-tree node, type {kind:#x}")));
    }

    let flags = le16(b, 32);
    let end = if flags & 1 != 0 { block - 40 } else { block };
    let toc = 56 + le16(b, 40) as usize;
    let keys = toc + le16(b, 42) as usize;
    let fixed = flags & 4 != 0;
    let count = le32(b, 36) as usize;
    let leaf = flags & 2 != 0;
    if keys > end || count > (keys - toc) / if fixed { 4 } else { 8 } || (!leaf && count == 0) {
      return Err(bad("APFS: a B-tree node is out of range"));
    }

    Ok(Self { b, count, toc, keys, end, leaf, fixed })
  }

  /// The offsets of entry `i`'s key and value.
  fn entry(&self, i: usize) -> (usize, usize) {
    match self.fixed {
      true => {
        (le16(self.b, self.toc + i * 4) as usize, le16(self.b, self.toc + i * 4 + 2) as usize)
      }
      false => {
        (le16(self.b, self.toc + i * 8) as usize, le16(self.b, self.toc + i * 8 + 4) as usize)
      }
    }
  }

  fn key(&self, off: usize, len: usize) -> io::Result<usize> {
    let at = self.keys + off;
    match at + len <= self.end {
      true => Ok(at),
      false => Err(bad("APFS: a key is out of range")),
    }
  }

  fn value(&self, off: usize, len: usize) -> io::Result<usize> {
    match off <= self.end && self.end - off >= self.keys && self.end - off + len <= self.end {
      true => Ok(self.end - off),
      false => Err(bad("APFS: a value is out of range")),
    }
  }

  /// A directory record's name at `at`, after its hash and length: the hash in the high 22 bits,
  /// the length with its NUL in the low 10.
  fn name(&self, at: usize) -> io::Result<(u32, &'a str)> {
    let x = le32(self.b, at);
    let (hash, len, start) = (x >> 10, (x & 0x3ff) as usize, at + 4);
    if len == 0 || start + len > self.end {
      return Err(bad("APFS: a name is out of range"));
    }

    let name = std::str::from_utf8(&self.b[start..start + len - 1]);
    Ok((hash, name.map_err(|_| bad("APFS: a name isn't UTF-8"))?))
  }
}

/// The UUID of the volume with `role` in the disk image, as `diskutil` names it.
pub fn uuid(disk: &Path, role: u16) -> io::Result<String> {
  let u = Container::open(disk, false)?.volume(role)?.uuid;
  let hex: String = u.iter().map(|x| format!("{x:02X}")).collect();
  Ok(format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..]))
}

/// Sets the owner of `paths`, relative to the volume's root, to root:wheel; each was written by a
/// mount that ignored ownership, or is root's already.
pub fn chown_root(disk: &Path, role: u16, paths: &[&str]) -> io::Result<()> {
  let c = Container::open(disk, true)?;
  let vol = c.volume(role)?;
  for path in paths {
    let mut id = ROOT_DIR;
    for name in path.split('/').filter(|x| !x.is_empty()) {
      id = c.child(&vol, id, name).map_err(|e| bad(format!("{path}: {e}")))?;
    }

    c.chown(&vol, id).map_err(|e| bad(format!("{path}: {e}")))?;
  }

  c.f.sync_all()
}

#[cfg(test)]
mod tests {
  use std::fs;
  use std::os::unix::fs::MetadataExt;
  use std::process::Command;

  use super::*;
  use crate::run;

  #[test]
  fn checksums() {
    let mut b = vec![0u8; 4096];
    b[8..16].copy_from_slice(&5u64.to_le_bytes());
    b[24] = 1;
    assert!(!valid(&b));
    let sum = fletcher(&b);
    b[..8].copy_from_slice(&sum.to_le_bytes());
    assert!(valid(&b));
    // The empty name's hash, and case folded.
    assert_eq!(name_hash(""), 0x3f_ffff);
    assert_eq!(name_hash("Library"), name_hash("library"));
  }

  /// A real APFS image: a file and a folder written by a mount that ignores ownership, their
  /// owner set, the volume checked by fsck_apfs and mounted with ownership honoured.
  #[test]
  fn chown() {
    let dir = std::env::temp_dir().join(format!("trivm-apfs-{}", std::process::id()));
    let (img, mnt) = (dir.join("disk.img"), dir.join("mnt"));
    fs::create_dir_all(&dir).unwrap();
    let mut create = Command::new("diskutil");
    create.args(["image", "create", "blank", "--size", "64m", "--fs", "APFS", "--volumeName", "T"]);
    run(create.arg(&img)).unwrap();

    let attach = |opts: &[&str]| {
      let mut cmd = Command::new("hdiutil");
      cmd.args(["attach", "-quiet", "-nobrowse", "-mountpoint"]).arg(&mnt).args(opts).arg(&img);
      run(&mut cmd).unwrap();
    };
    let detach = || run(Command::new("hdiutil").args(["detach", "-force"]).arg(&mnt)).unwrap();

    attach(&[]);
    fs::create_dir_all(mnt.join("a b")).unwrap();
    fs::write(mnt.join("a b/file.plist"), "x").unwrap();
    detach();

    assert!(chown_root(&img, 0, &["a b/missing"]).is_err());
    chown_root(&img, 0, &["a b", "a b/file.plist"]).unwrap();
    // Root's already: nothing to do.
    chown_root(&img, 0, &["a b/file.plist"]).unwrap();
    assert_eq!(uuid(&img, 0).unwrap().len(), 36);
    assert!(uuid(&img, DATA).is_err());

    let out = run(Command::new("hdiutil").args(["attach", "-nomount"]).arg(&img)).unwrap();
    let out = String::from_utf8_lossy(&out);
    let dev = out.split_whitespace().next().unwrap();
    let fsck = run(Command::new("fsck_apfs").args(["-n", dev]));
    run(Command::new("hdiutil").args(["detach", "-force", dev])).unwrap();
    fsck.unwrap();

    attach(&["-owners", "on"]);
    let owners = |x: &str| fs::metadata(mnt.join(x)).map(|x| (x.uid(), x.gid()));
    let (dir_owner, file_owner) = (owners("a b"), owners("a b/file.plist"));
    detach();
    assert_eq!(dir_owner.unwrap(), (0, 0));
    assert_eq!(file_owner.unwrap(), (0, 0));
    fs::remove_dir_all(&dir).unwrap();
  }
}
