// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! qcow2 to a raw disk, which is what VZ reads: the distros ship their arm64 cloud disks as
//! qcow2 only. Reads version 2 and 3 images without a backing file or encryption, clusters plain
//! or deflated, as QEMU's docs/interop/qcow2.rst describes them; the raw file is sparse.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::FileExt;
use std::path::Path;

use crate::bad;

const MAGIC: u32 = 0x5146_49fb;
const OFFSET: u64 = 0x00ff_ffff_ffff_fe00;
const COMPRESSED: u64 = 1 << 62;
const ZERO: u64 = 1;

fn be32(b: &[u8], at: usize) -> u32 {
  u32::from_be_bytes(b[at..at + 4].try_into().unwrap_or_default())
}

fn be64(b: &[u8], at: usize) -> u64 {
  u64::from_be_bytes(b[at..at + 8].try_into().unwrap_or_default())
}

/// Writes the qcow2 image `src` to `dst` as a raw disk of its virtual size.
pub fn to_raw(src: &Path, dst: &Path) -> io::Result<()> {
  let mut f = File::open(src)?;
  let mut head = [0u8; 112];
  f.read_exact(&mut head[..72])?;

  let version = be32(&head, 4);
  if be32(&head, 0) != MAGIC || !(2..=3).contains(&version) {
    return Err(bad(format!("{}: not a qcow2 image", src.display())));
  }

  if be64(&head, 8) != 0 || be32(&head, 32) != 0 {
    return Err(bad(format!("{}: a backing file or encryption", src.display())));
  }

  if version == 3 {
    f.read_exact(&mut head[72..104])?;
    let len = be32(&head, 100) as usize;
    if len > 104 {
      f.read_exact(&mut head[104..len.min(112)])?;
    }

    // Only the dirty bit is fine to read past: the others are a corrupt image, an external data
    // file, zstd and extended L2 entries.
    if be64(&head, 72) & !1 != 0 || (len > 104 && head[104] != 0) {
      return Err(bad(format!("{}: qcow2 features not supported", src.display())));
    }
  }

  let bits = be32(&head, 20);
  let size = be64(&head, 24);
  let cluster = 1u64 << bits;
  let per_l2 = cluster / 8;

  let mut l1 = vec![0u8; be32(&head, 36) as usize * 8];
  f.read_exact_at(&mut l1, be64(&head, 40))?;

  let out = File::create(dst)?;
  out.set_len(size)?;
  let mut l2 = vec![0u8; cluster as usize];
  let mut data = vec![0u8; cluster as usize];
  for (i, x) in l1.chunks(8).enumerate() {
    let table = be64(x, 0) & OFFSET;
    if table == 0 {
      continue;
    }

    f.read_exact_at(&mut l2, table)?;
    for (j, x) in l2.chunks(8).enumerate() {
      let entry = be64(x, 0);
      let at = (i as u64 * per_l2 + j as u64) * cluster;
      if at >= size || entry & !(1 << 63) == 0 {
        continue;
      }

      let len = cluster.min(size - at) as usize;
      if entry & COMPRESSED != 0 {
        // The offset's bits, then the count of 512-byte sectors after the first.
        let x = 62 - (bits - 8);
        let host = entry & ((1 << x) - 1);
        let sectors = (entry >> x) & ((1 << (bits - 8)) - 1);
        let mut packed = vec![0u8; ((sectors + 1) * 512 - (host & 511)) as usize];
        let read = read_at(&mut f, &mut packed, host)?;
        let raw =
          miniz_oxide::inflate::decompress_to_vec_with_limit(&packed[..read], cluster as usize);
        let raw =
          raw.map_err(|e| bad(format!("{}: a cluster doesn't inflate: {e:?}", src.display())))?;
        if raw.iter().any(|x| *x != 0) {
          out.write_all_at(&raw[..len.min(raw.len())], at)?;
        }

        continue;
      }

      let host = entry & OFFSET;
      if entry & ZERO != 0 || host == 0 {
        continue;
      }

      f.read_exact_at(&mut data[..len], host)?;
      if data[..len].iter().any(|x| *x != 0) {
        out.write_all_at(&data[..len], at)?;
      }
    }
  }

  Ok(())
}

/// Reads what there is at `at`, up to `buf`'s size: a compressed cluster's sectors may run past
/// the file's end.
fn read_at(f: &mut File, buf: &mut [u8], at: u64) -> io::Result<usize> {
  f.seek(SeekFrom::Start(at))?;
  let mut n = 0;
  while n < buf.len() {
    match f.read(&mut buf[n..])? {
      0 => break,
      x => n += x,
    }
  }

  Ok(n)
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A qcow2 image of 4 clusters of 512 bytes: a plain one, a deflated one, a zero one, and one
  /// never written.
  fn image() -> Vec<u8> {
    let bits = 9u32;
    let mut img = vec![0u8; 512 * 4];
    img[0..4].copy_from_slice(&MAGIC.to_be_bytes());
    img[4..8].copy_from_slice(&2u32.to_be_bytes());
    img[20..24].copy_from_slice(&bits.to_be_bytes());
    img[24..32].copy_from_slice(&(512u64 * 4).to_be_bytes());
    img[36..40].copy_from_slice(&1u32.to_be_bytes());
    img[40..48].copy_from_slice(&512u64.to_be_bytes());
    // L1 at 512 points at the L2 at 1024; the plain cluster at 1536.
    img[512..520].copy_from_slice(&(1024u64 | 1 << 63).to_be_bytes());
    img[1024..1032].copy_from_slice(&1536u64.to_be_bytes());
    img[1536..2048].fill(0xab);

    let packed = miniz_oxide::deflate::compress_to_vec(&[0xcd; 512], 6);
    let host = img.len() as u64;
    img.extend_from_slice(&packed);
    let x = 62 - (bits - 8);
    let entry = COMPRESSED | host | ((packed.len() as u64 / 512) << x);
    img[1032..1040].copy_from_slice(&entry.to_be_bytes());
    img[1040..1048].copy_from_slice(&(ZERO | 1536).to_be_bytes());
    img
  }

  #[test]
  fn raw() {
    let dir = std::env::temp_dir().join(format!("trivm-qcow2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.qcow2"), image()).unwrap();
    to_raw(&dir.join("a.qcow2"), &dir.join("a.raw")).unwrap();

    let raw = std::fs::read(dir.join("a.raw")).unwrap();
    assert_eq!(raw.len(), 2048);
    assert!(raw[..512].iter().all(|x| *x == 0xab));
    assert!(raw[512..1024].iter().all(|x| *x == 0xcd));
    assert!(raw[1024..].iter().all(|x| *x == 0));

    std::fs::write(
      dir.join("b.qcow2"),
      b"nope, not an image at all, though long enough to read the header!!!!!!",
    )
    .unwrap();
    assert!(to_raw(&dir.join("b.qcow2"), &dir.join("b.raw")).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
  }
}
