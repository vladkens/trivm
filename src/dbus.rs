// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! D-Bus's wire protocol, the little of it trivm needs to feed QEMU's microphone (`mic.rs`): a
//! connection to a peer over a Unix socket, the client's side of the EXTERNAL authentication,
//! method calls that may carry a Unix fd, their returns and errors, in little-endian. After
//! https://dbus.freedesktop.org/doc/dbus-specification.html.

use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use crate::bad;

/// A message's types.
const CALL: u8 = 1;
const RETURN: u8 = 2;
const ERROR: u8 = 3;

/// A call's flag: no reply is wanted.
const NO_REPLY: u8 = 1;

/// A message as read: the header's fields trivm uses and the body.
#[derive(Debug, Default, PartialEq)]
pub struct Msg {
  pub kind: u8,
  pub flags: u8,
  pub serial: u32,
  pub reply_to: u32,
  pub member: String,
  pub error: String,
  pub sig: String,
  pub body: Vec<u8>,
}

impl Msg {
  pub fn wants_reply(&self) -> bool {
    self.kind == CALL && self.flags & NO_REPLY == 0
  }

  /// The body's values, read in order.
  pub fn args(&self) -> Args<'_> {
    Args { data: &self.body, at: 0 }
  }
}

/// A header's field, by its code.
enum Field<'a> {
  Path(&'a str),
  Interface(&'a str),
  Member(&'a str),
  Error(&'a str),
  ReplyTo(u32),
  Sig(&'a str),
  Fds(u32),
}

/// Bytes written with D-Bus's alignment, counted from the message's start.
#[derive(Default)]
pub struct Buf(pub Vec<u8>);

impl Buf {
  fn align(&mut self, n: usize) {
    self.0.resize(self.0.len().next_multiple_of(n), 0);
  }

  pub fn u32(&mut self, x: u32) {
    self.align(4);
    self.0.extend(x.to_le_bytes());
  }

  fn str(&mut self, x: &str) {
    self.u32(x.len() as u32);
    self.0.extend(x.as_bytes());
    self.0.push(0);
  }

  fn sig(&mut self, x: &str) {
    self.0.push(x.len() as u8);
    self.0.extend(x.as_bytes());
    self.0.push(0);
  }

  /// An array of bytes, `ay`.
  pub fn bytes(&mut self, x: &[u8]) {
    self.u32(x.len() as u32);
    self.0.extend(x);
  }
}

/// A body's values, read with D-Bus's alignment.
pub struct Args<'a> {
  data: &'a [u8],
  at: usize,
}

impl Args<'_> {
  fn take(&mut self, n: usize) -> io::Result<&[u8]> {
    self.at = self.at.next_multiple_of(n);
    let x = self.data.get(self.at..self.at + n).ok_or_else(|| bad("a D-Bus message cut short"))?;
    self.at += n;
    Ok(x)
  }

  pub fn u8(&mut self) -> io::Result<u8> {
    Ok(self.take(1)?[0])
  }

  pub fn u32(&mut self) -> io::Result<u32> {
    Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap_or_default()))
  }

  pub fn u64(&mut self) -> io::Result<u64> {
    Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap_or_default()))
  }

  pub fn bool(&mut self) -> io::Result<bool> {
    Ok(self.u32()? != 0)
  }

  /// An array of bytes, `ay`.
  pub fn bytes(&mut self) -> io::Result<Vec<u8>> {
    let n = self.u32()? as usize;
    (0..n).map(|_| self.u8()).collect()
  }

  fn str(&mut self) -> io::Result<String> {
    let n = self.u32()? as usize;
    self.text(n)
  }

  fn sig(&mut self) -> io::Result<String> {
    let n = self.u8()? as usize;
    self.text(n)
  }

  /// `n` bytes of text and the 0 after them.
  fn text(&mut self, n: usize) -> io::Result<String> {
    let x = self.data.get(self.at..self.at + n).ok_or_else(|| bad("a D-Bus message cut short"))?;
    self.at += n + 1;
    Ok(String::from_utf8_lossy(x).into_owned())
  }
}

/// A message's bytes: the fixed header, the fields, the body.
fn encode(kind: u8, serial: u32, fields: &[Field], body: &[u8]) -> Vec<u8> {
  // The fields' array starts at 16, 8-aligned: counted with the header before them.
  let mut f = Buf(vec![0; 16]);
  for x in fields {
    f.align(8);
    let (code, sig) = match x {
      Field::Path(_) => (1, "o"),
      Field::Interface(_) => (2, "s"),
      Field::Member(_) => (3, "s"),
      Field::Error(_) => (4, "s"),
      Field::ReplyTo(_) => (5, "u"),
      Field::Sig(_) => (8, "g"),
      Field::Fds(_) => (9, "u"),
    };

    f.0.push(code);
    f.sig(sig);
    match x {
      Field::Path(x) | Field::Interface(x) | Field::Member(x) | Field::Error(x) => f.str(x),
      Field::ReplyTo(x) | Field::Fds(x) => f.u32(*x),
      Field::Sig(x) => f.sig(x),
    }
  }

  let len = f.0.len() - 16;
  let mut m = Buf(vec![b'l', kind, 0, 1]);
  m.u32(body.len() as u32);
  m.u32(serial);
  m.u32(len as u32);
  m.0.extend(&f.0[16..]);
  m.align(8);
  m.0.extend(body);
  m.0
}

/// The first whole message in `data` and its length, if one is there.
fn decode(data: &[u8]) -> io::Result<Option<(Msg, usize)>> {
  if data.len() < 16 {
    return Ok(None);
  }

  if data[0] != b'l' {
    return Err(bad("a big-endian D-Bus message"));
  }

  let u32_at = |at: usize| u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
  let (body_len, len) = (u32_at(4) as usize, u32_at(12) as usize);
  let body = (16 + len).next_multiple_of(8);
  if data.len() < body + body_len {
    return Ok(None);
  }

  let mut msg = Msg { kind: data[1], flags: data[2], serial: u32_at(8), ..Default::default() };
  let mut f = Args { data: &data[..16 + len], at: 16 };
  while f.at.next_multiple_of(8) < 16 + len {
    f.at = f.at.next_multiple_of(8);
    let code = f.u8()?;
    let sig = f.sig()?;
    match (code, sig.as_str()) {
      (3, "s") => msg.member = f.str()?,
      (4, "s") => msg.error = f.str()?,
      (5, "u") => msg.reply_to = f.u32()?,
      (8, "g") => msg.sig = f.sig()?,
      (_, "o" | "s") => _ = f.str()?,
      (_, "u") => _ = f.u32()?,
      (_, "g") => _ = f.sig()?,
      _ => return Err(bad(format!("a D-Bus header field of type {sig}"))),
    }
  }

  msg.body = data[body..body + body_len].to_vec();
  Ok(Some((msg, body + body_len)))
}

/// A connection to a peer, as its client.
pub struct Conn {
  sock: UnixStream,
  buf: Vec<u8>,
  serial: u32,
}

impl Conn {
  /// Authenticates as the user (EXTERNAL), asking to pass Unix fds when `fds`.
  pub fn client(mut sock: UnixStream, fds: bool) -> io::Result<Self> {
    // SAFETY: getuid has no preconditions and can't fail.
    let uid = unsafe { getuid() }.to_string();
    let uid: String = uid.bytes().map(|x| format!("{x:02x}")).collect();
    sock.write_all(format!("\0AUTH EXTERNAL {uid}\r\n").as_bytes())?;
    expect(&mut sock, "OK ")?;
    if fds {
      sock.write_all(b"NEGOTIATE_UNIX_FD\r\n")?;
      expect(&mut sock, "AGREE_UNIX_FD")?;
    }

    sock.write_all(b"BEGIN\r\n")?;
    Ok(Self { sock, buf: Vec::new(), serial: 0 })
  }

  fn send(
    &mut self,
    kind: u8,
    fields: &[Field],
    body: &[u8],
    fd: Option<RawFd>,
  ) -> io::Result<u32> {
    self.serial += 1;
    let msg = encode(kind, self.serial, fields, body);
    match fd {
      Some(fd) => send_fd(&self.sock, &msg, fd)?,
      None => self.sock.write_all(&msg)?,
    }

    Ok(self.serial)
  }

  /// Calls a method, its body's signature `sig`, an fd's index in it passing `fd`; returns the
  /// call's serial.
  pub fn call(
    &mut self,
    path: &str,
    interface: &str,
    member: &str,
    sig: &str,
    body: &[u8],
    fd: Option<RawFd>,
  ) -> io::Result<u32> {
    let mut fields = vec![Field::Path(path), Field::Interface(interface), Field::Member(member)];
    if !sig.is_empty() {
      fields.push(Field::Sig(sig));
    }

    if fd.is_some() {
      fields.push(Field::Fds(1));
    }

    self.send(CALL, &fields, body, fd)
  }

  pub fn reply(&mut self, to: &Msg, sig: &str, body: &[u8]) -> io::Result<()> {
    let mut fields = vec![Field::ReplyTo(to.serial)];
    if !sig.is_empty() {
      fields.push(Field::Sig(sig));
    }

    self.send(RETURN, &fields, body, None).map(|_| ())
  }

  pub fn error(&mut self, to: &Msg, name: &str) -> io::Result<()> {
    self.send(ERROR, &[Field::ReplyTo(to.serial), Field::Error(name)], &[], None).map(|_| ())
  }

  /// Sets how long `next` waits for a message.
  pub fn timeout(&self, x: Duration) -> io::Result<()> {
    self.sock.set_read_timeout(Some(x))
  }

  /// The next message, or none when the socket's read timeout passes first; `UnexpectedEof` once
  /// the peer has closed the connection.
  pub fn next(&mut self) -> io::Result<Option<Msg>> {
    loop {
      if let Some((msg, n)) = decode(&self.buf)? {
        self.buf.drain(..n);
        return Ok(Some(msg));
      }

      let mut chunk = [0; 65536];
      match self.sock.read(&mut chunk) {
        Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "D-Bus peer gone")),
        Ok(n) => self.buf.extend(&chunk[..n]),
        Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
          return Ok(None);
        }
        Err(e) => return Err(e),
      }
    }
  }

  /// The reply to the call of this serial, the messages before it dropped; an error reply as an
  /// error.
  pub fn wait(&mut self, serial: u32) -> io::Result<Msg> {
    loop {
      let Some(msg) = self.next()? else { return Err(bad("no D-Bus reply in time")) };
      match msg.kind {
        RETURN if msg.reply_to == serial => return Ok(msg),
        ERROR if msg.reply_to == serial => {
          let text = msg.args().str().unwrap_or_default();
          return Err(bad(format!("{}: {text}", msg.error)));
        }
        _ => {}
      }
    }
  }
}

/// Reads the server's line of the authentication, which must start so.
fn expect(sock: &mut UnixStream, start: &str) -> io::Result<()> {
  let mut line = Vec::new();
  let mut byte = [0];
  while !line.ends_with(b"\r\n") {
    if sock.read(&mut byte)? == 0 {
      return Err(bad("the D-Bus peer closed the connection"));
    }

    line.push(byte[0]);
  }

  let line = String::from_utf8_lossy(&line);
  match line.starts_with(start) {
    true => Ok(()),
    false => Err(bad(format!("D-Bus authentication: {}", line.trim()))),
  }
}

#[repr(C)]
struct IoVec {
  base: *const u8,
  len: usize,
}

#[repr(C)]
struct MsgHdr {
  name: *mut u8,
  namelen: u32,
  iov: *const IoVec,
  iovlen: i32,
  control: *mut u8,
  controllen: u32,
  flags: i32,
}

unsafe extern "C" {
  fn sendmsg(fd: i32, msg: *const MsgHdr, flags: i32) -> isize;
  fn getuid() -> u32;
}

/// macOS's `SOL_SOCKET` and `SCM_RIGHTS`.
const SOL_SOCKET: i32 = 0xffff;
const SCM_RIGHTS: i32 = 1;

/// Writes `data` with an fd passed beside its first byte (`SCM_RIGHTS`).
pub fn send_fd(sock: &UnixStream, data: &[u8], fd: RawFd) -> io::Result<()> {
  // cmsghdr (length, level, type) and the fd: CMSG_SPACE(4) on macOS, 4-aligned.
  let mut control = [0u8; 16];
  control[..4].copy_from_slice(&16u32.to_ne_bytes());
  control[4..8].copy_from_slice(&SOL_SOCKET.to_ne_bytes());
  control[8..12].copy_from_slice(&SCM_RIGHTS.to_ne_bytes());
  control[12..].copy_from_slice(&fd.to_ne_bytes());
  let iov = IoVec { base: data.as_ptr(), len: data.len() };
  let msg = MsgHdr {
    name: std::ptr::null_mut(),
    namelen: 0,
    iov: &iov,
    iovlen: 1,
    control: control.as_mut_ptr(),
    controllen: 16,
    flags: 0,
  };

  // SAFETY: the header points at `iov`, `data` and `control`, which live through the call.
  let n = unsafe { sendmsg(sock.as_raw_fd(), &msg, 0) };
  if n < 0 {
    return Err(io::Error::last_os_error());
  }

  // The fd went with the first bytes; the rest is written as usual.
  (&*sock).write_all(&data[n as usize..])
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn messages() {
    let fields = [
      Field::Path("/org/qemu/Display1/Audio"),
      Field::Interface("org.qemu.Display1.Audio"),
      Field::Member("RegisterInListener"),
      Field::Sig("h"),
      Field::Fds(1),
    ];
    let msg = encode(CALL, 7, &fields, &[0, 0, 0, 0]);
    assert_eq!(&msg[..12], [b'l', 1, 0, 1, 4, 0, 0, 0, 7, 0, 0, 0]);
    assert_eq!(msg.len() % 8, 4);
    let (back, n) = decode(&msg).unwrap().unwrap();
    assert_eq!(n, msg.len());
    assert_eq!(
      (back.kind, back.serial, back.member.as_str(), back.sig.as_str()),
      (1, 7, "RegisterInListener", "h")
    );
    assert!(back.wants_reply());
    assert_eq!(decode(&msg[..msg.len() - 1]).unwrap(), None);

    let mut body = Buf::default();
    body.bytes(&[1, 2, 3]);
    let msg = encode(RETURN, 8, &[Field::ReplyTo(5), Field::Sig("ay")], &body.0);
    let (back, _) = decode(&msg).unwrap().unwrap();
    assert_eq!(
      (back.kind, back.reply_to, back.body.as_slice()),
      (RETURN, 5, &[3, 0, 0, 0, 1, 2, 3][..])
    );

    // QEMU's Init(t y b b u y u u b): 48 kHz mono, 16-bit signed.
    let mut body = Buf(42u64.to_le_bytes().to_vec());
    body.0.push(16);
    for x in [1, 0, 48_000] {
      body.u32(x);
    }

    body.0.push(1);
    for x in [2, 96_000, 0] {
      body.u32(x);
    }

    let msg = Msg { body: body.0, ..Default::default() };
    let mut a = msg.args();
    assert_eq!(
      (a.u64().unwrap(), a.u8().unwrap(), a.bool().unwrap(), a.bool().unwrap()),
      (42, 16, true, false)
    );
    assert_eq!((a.u32().unwrap(), a.u8().unwrap(), a.u32().unwrap()), (48_000, 1, 2));
    assert_eq!((a.u32().unwrap(), a.bool().unwrap()), (96_000, false));
    assert!(a.u32().is_err());

    // QEMU's SetVolume(t b ay): unmuted, a level a channel.
    let mut body = Buf(42u64.to_le_bytes().to_vec());
    body.u32(0);
    body.bytes(&[255, 128]);
    let msg = Msg { body: body.0, ..Default::default() };
    let mut a = msg.args();
    assert_eq!((a.u64().unwrap(), a.bool().unwrap()), (42, false));
    assert_eq!(a.bytes().unwrap(), [255, 128]);
    assert!(a.u8().is_err());
  }
}
