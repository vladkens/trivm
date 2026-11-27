// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! Virtualization.framework: a VM's configuration, the platform files and the install of macOS,
//! and the process that runs a VM (`trivm _run`) and answers trivm's requests on `ctl.sock`.

use std::cell::RefCell;
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{self, Command};
use std::sync::mpsc;
use std::time::Duration;
use std::{fs, thread};

use block2::RcBlock;
use dispatch2::run_on_main;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{AllocAnyThread, MainThreadMarker, msg_send};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::{NSArray, NSData, NSDate, NSError, NSRunLoop, NSString, NSURL};
use objc2_virtualization::*;

use crate::bad;
use crate::display::{self, Act, Display};
use crate::vm::{Os, PASSWORD, USER, Vm};

pub fn url(path: &Path) -> Retained<NSURL> {
  NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))
}

fn ns(e: Retained<NSError>) -> io::Error {
  bad(e.localizedDescription().to_string())
}

fn message(e: *mut NSError) -> String {
  // SAFETY: VZ's completion handlers pass a valid error or null.
  match unsafe { e.as_ref() } {
    Some(e) => e.localizedDescription().to_string(),
    None => "unknown error".into(),
  }
}

/// Runs the main run loop for a moment, so the main queue's work (VZ's callbacks) is done.
pub fn spin(secs: f64) {
  NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(secs));
  // Without input sources the run loop returns at once.
  thread::sleep(Duration::from_millis(10));
}

fn wait<T>(rx: &mpsc::Receiver<T>) -> T {
  loop {
    if let Ok(x) = rx.try_recv() {
      return x;
    }

    spin(0.1);
  }
}

// MARK: Configuration

pub fn config(vm: &Vm) -> io::Result<Retained<VZVirtualMachineConfiguration>> {
  let os = vm.config.os();
  if os == Os::Windows {
    return Err(bad(format!("{} runs on QEMU", vm.name)));
  }

  let screen = vm.config.screen();
  let (w, h) = ((screen.width * screen.scale) as isize, (screen.height * screen.scale) as isize);

  // SAFETY: plain configuration objects, used on this thread only.
  unsafe {
    let c = VZVirtualMachineConfiguration::new();
    c.setCPUCount(vm.config.cpus as usize);
    c.setMemorySize(vm.config.memory_bytes());

    let mut storage = vec![disk(&vm.path("disk.img"), false, os)?];

    let net = VZVirtioNetworkDeviceConfiguration::new();
    net.setAttachment(Some(&VZNATNetworkDeviceAttachment::new()));
    let mac = VZMACAddress::initWithString(VZMACAddress::alloc(), &NSString::from_str(&vm.mac));
    let mac = mac.ok_or_else(|| bad(format!("{}: not a MAC address", vm.path("mac").display())))?;
    net.setMACAddress(&mac);
    c.setNetworkDevices(&NSArray::from_retained_slice(&[Retained::into_super(net)]));

    let entropy = VZVirtioEntropyDeviceConfiguration::new();
    c.setEntropyDevices(&NSArray::from_retained_slice(&[Retained::into_super(entropy)]));
    let balloon = VZVirtioTraditionalMemoryBalloonDeviceConfiguration::new();
    c.setMemoryBalloonDevices(&NSArray::from_retained_slice(&[Retained::into_super(balloon)]));

    let pointer = Retained::into_super(VZUSBScreenCoordinatePointingDeviceConfiguration::new());
    match os {
      Os::Macos => {
        let data = |file: &str| -> io::Result<Retained<NSData>> {
          Ok(NSData::with_bytes(&fs::read(vm.path(file))?))
        };
        let hw = data("hardware.bin")?;
        let hw = VZMacHardwareModel::initWithDataRepresentation(VZMacHardwareModel::alloc(), &hw);
        let hw = hw.ok_or_else(|| bad("hardware.bin: not a hardware model"))?;
        if !hw.isSupported() {
          return Err(bad("this Mac can't run this VM's hardware model"));
        }

        let id = data("machine.bin")?;
        let id =
          VZMacMachineIdentifier::initWithDataRepresentation(VZMacMachineIdentifier::alloc(), &id);
        let id = id.ok_or_else(|| bad("machine.bin: not a machine identifier"))?;
        let aux = VZMacAuxiliaryStorage::initWithURL(
          VZMacAuxiliaryStorage::alloc(),
          &url(&vm.path("aux.img")),
        );
        let p = VZMacPlatformConfiguration::new();
        p.setHardwareModel(&hw);
        p.setMachineIdentifier(&id);
        p.setAuxiliaryStorage(Some(&aux));
        c.setPlatform(&p);
        c.setBootLoader(Some(&VZMacOSBootLoader::new()));

        let d =
          VZMacGraphicsDisplayConfiguration::initWithWidthInPixels_heightInPixels_pixelsPerInch(
            VZMacGraphicsDisplayConfiguration::alloc(),
            w,
            h,
            // 220 at 2x, a Retina density, at which macOS draws a point as 2 pixels; 110 at 1x.
            (110.0 * screen.scale) as isize,
          );
        let g = VZMacGraphicsDeviceConfiguration::new();
        g.setDisplays(&NSArray::from_retained_slice(&[d]));
        c.setGraphicsDevices(&NSArray::from_retained_slice(&[Retained::into_super(g)]));

        let keyboard = Retained::into_super(VZMacKeyboardConfiguration::new());
        c.setKeyboards(&NSArray::from_retained_slice(&[keyboard]));
        let trackpad = Retained::into_super(VZMacTrackpadConfiguration::new());
        c.setPointingDevices(&NSArray::from_retained_slice(&[pointer, trackpad]));
      }
      Os::Linux | Os::Windows => {
        let id = VZGenericMachineIdentifier::initWithDataRepresentation(
          VZGenericMachineIdentifier::alloc(),
          &NSData::with_bytes(&fs::read(vm.path("machine.bin"))?),
        );
        let id = id.ok_or_else(|| bad("machine.bin: not a machine identifier"))?;
        let p = VZGenericPlatformConfiguration::new();
        p.setMachineIdentifier(&id);
        c.setPlatform(&p);

        let store =
          VZEFIVariableStore::initWithURL(VZEFIVariableStore::alloc(), &url(&vm.path("efi.bin")));
        let boot = VZEFIBootLoader::new();
        boot.setVariableStore(Some(&store));
        c.setBootLoader(Some(&boot));

        let scanout = VZVirtioGraphicsScanoutConfiguration::initWithWidthInPixels_heightInPixels(
          VZVirtioGraphicsScanoutConfiguration::alloc(),
          w,
          h,
        );
        let g = VZVirtioGraphicsDeviceConfiguration::new();
        g.setScanouts(&NSArray::from_retained_slice(&[scanout]));
        c.setGraphicsDevices(&NSArray::from_retained_slice(&[Retained::into_super(g)]));

        let keyboard = Retained::into_super(VZUSBKeyboardConfiguration::new());
        c.setKeyboards(&NSArray::from_retained_slice(&[keyboard]));
        c.setPointingDevices(&NSArray::from_retained_slice(&[pointer]));

        if vm.path("seed.iso").exists() {
          storage.push(disk(&vm.path("seed.iso"), true, Os::Linux)?);
        }

        let console = VZFileSerialPortAttachment::initWithURL_append_error(
          VZFileSerialPortAttachment::alloc(),
          &url(&vm.state("console.log")),
          true,
        )
        .map_err(ns)?;
        let serial = VZVirtioConsoleDeviceSerialPortConfiguration::new();
        serial.setAttachment(Some(&console));
        c.setSerialPorts(&NSArray::from_retained_slice(&[Retained::into_super(serial)]));
      }
    }

    c.setStorageDevices(&NSArray::from_retained_slice(&storage));
    c.validateWithError().map_err(ns)?;
    Ok(c)
  }
}

/// A virtio disk. Linux's is cached by the host and synced with fsync, as Lima has it: with VZ's
/// defaults (automatic, full) ext4 lost files on the first boot here.
fn disk(
  path: &Path,
  read_only: bool,
  os: Os,
) -> io::Result<Retained<VZStorageDeviceConfiguration>> {
  let (cache, sync) = match os {
    Os::Macos => (VZDiskImageCachingMode::Automatic, VZDiskImageSynchronizationMode::Full),
    Os::Linux | Os::Windows => {
      (VZDiskImageCachingMode::Cached, VZDiskImageSynchronizationMode::Fsync)
    }
  };

  // SAFETY: a configuration object for a file that exists.
  unsafe {
    let a = VZDiskImageStorageDeviceAttachment::initWithURL_readOnly_cachingMode_synchronizationMode_error(
      VZDiskImageStorageDeviceAttachment::alloc(),
      &url(path),
      read_only,
      cache,
      sync,
    )
    .map_err(|e| bad(format!("{}: {}", path.display(), e.localizedDescription())))?;
    let d = VZVirtioBlockDeviceConfiguration::initWithAttachment(
      VZVirtioBlockDeviceConfiguration::alloc(),
      &a,
    );
    Ok(Retained::into_super(d))
  }
}

// MARK: Platform files

/// macOS's `hardware.bin`, `machine.bin` and `aux.img`, for the hardware model the IPSW asks
/// for; checks that this Mac runs it.
pub fn mac_platform(vm: &Vm, ipsw: &Path) -> io::Result<()> {
  let (tx, rx) = mpsc::channel();
  let block = RcBlock::new(move |image: *mut VZMacOSRestoreImage, e: *mut NSError| {
    // SAFETY: a valid image or null, as the error says.
    let res = match unsafe { image.as_ref() } {
      None => Err(message(e)),
      Some(image) => match unsafe { image.mostFeaturefulSupportedConfiguration() } {
        None => Err("this Mac can't run this macOS".to_string()),
        Some(req) => Ok(unsafe { req.hardwareModel().dataRepresentation() }.to_vec()),
      },
    };

    let _ = tx.send(res);
  });

  // SAFETY: the block lives until it is called; `wait` runs the queue that calls it.
  unsafe { VZMacOSRestoreImage::loadFileURL_completionHandler(&url(ipsw), &block) };
  let hw = wait(&rx).map_err(|e| bad(format!("{}: {e}", ipsw.display())))?;

  // SAFETY: plain objects from data just read.
  unsafe {
    let model = VZMacHardwareModel::initWithDataRepresentation(
      VZMacHardwareModel::alloc(),
      &NSData::with_bytes(&hw),
    );
    let model = model.ok_or_else(|| bad("the IPSW's hardware model can't be read"))?;
    VZMacAuxiliaryStorage::initCreatingStorageAtURL_hardwareModel_options_error(
      VZMacAuxiliaryStorage::alloc(),
      &url(&vm.path("aux.img")),
      &model,
      VZMacAuxiliaryStorageInitializationOptions::AllowOverwrite,
    )
    .map_err(ns)?;
  }

  fs::write(vm.path("machine.bin"), machine_id(Os::Macos))?;
  fs::write(vm.path("hardware.bin"), hw)
}

/// Linux's `machine.bin` and `efi.bin`.
pub fn linux_platform(vm: &Vm) -> io::Result<()> {
  fs::write(vm.path("machine.bin"), machine_id(Os::Linux))?;
  // SAFETY: a plain object, made at a path in the VM's folder.
  unsafe {
    VZEFIVariableStore::initCreatingVariableStoreAtURL_options_error(
      VZEFIVariableStore::alloc(),
      &url(&vm.path("efi.bin")),
      VZEFIVariableStoreInitializationOptions::AllowOverwrite,
    )
    .map_err(ns)?;
  }

  Ok(())
}

/// A new machine identifier, `machine.bin`: Apple asks VMs that run at once to have their own.
pub fn machine_id(os: Os) -> Vec<u8> {
  // SAFETY: plain objects.
  unsafe {
    match os {
      Os::Macos => VZMacMachineIdentifier::new().dataRepresentation().to_vec(),
      Os::Linux | Os::Windows => VZGenericMachineIdentifier::new().dataRepresentation().to_vec(),
    }
  }
}

/// Installs macOS from the IPSW on the VM's disk, printing the progress.
pub fn install(vm: &Vm, ipsw: &Path) -> io::Result<()> {
  let config = config(vm)?;
  let (tx, rx) = mpsc::channel();
  let block = RcBlock::new(move |e: *mut NSError| {
    let _ = tx.send(if e.is_null() { Ok(()) } else { Err(message(e)) });
  });

  // SAFETY: the VM runs on the main queue, which `spin` runs on this, the main thread.
  let installer = unsafe {
    let machine = VZVirtualMachine::initWithConfiguration(VZVirtualMachine::alloc(), &config);
    let installer = VZMacOSInstaller::initWithVirtualMachine_restoreImageURL(
      VZMacOSInstaller::alloc(),
      &machine,
      &url(ipsw),
    );
    installer.installWithCompletionHandler(&block);
    installer
  };

  let progress = unsafe { installer.progress() };
  let mut shown = -1;
  loop {
    if let Ok(x) = rx.try_recv() {
      return x.map_err(|e| bad(format!("installing macOS: {e}")));
    }

    let done = (progress.fractionCompleted() * 100.0) as i32;
    if done / 10 != shown / 10 {
      eprintln!("installing macOS: {done}%");
      shown = done;
    }

    spin(0.5);
  }
}

// MARK: The VM's process

struct State {
  machine: Retained<VZVirtualMachine>,
  display: Display,
}

thread_local! {
  /// The running VM, on the main thread.
  static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Runs `f` with the VM's state on the main thread.
fn main<R: Send>(
  f: impl FnOnce(&mut State, MainThreadMarker) -> io::Result<R> + Send,
) -> io::Result<R> {
  run_on_main(move |mtm| {
    STATE.with(|s| match s.borrow_mut().as_mut() {
      Some(s) => f(s, mtm),
      None => Err(bad("the VM isn't set up yet")),
    })
  })
}

/// The first boot's options for macOS 27+: the user, logged in automatically, with Remote Login.
/// `VZMacGuestProvisioningOptions` is newer than objc2-virtualization 0.3.2.
fn provisioning() -> io::Result<Retained<VZMacOSVirtualMachineStartOptions>> {
  let cls = AnyClass::get(c"VZMacGuestProvisioningOptions");
  let cls = cls.ok_or_else(|| bad("making the guest's user needs macOS 27 on this Mac"))?;

  // SAFETY: the methods and types of VZMacGuestProvisioningOptions.h in the macOS 27 SDK.
  unsafe {
    let p: Retained<AnyObject> = msg_send![cls, new];
    let _: () = msg_send![&*p, setFullName: &*NSString::from_str("User")];
    let _: () = msg_send![&*p, setUsername: &*NSString::from_str(USER)];
    let _: () = msg_send![&*p, setPassword: &*NSString::from_str(PASSWORD)];
    let _: () = msg_send![&*p, setLogsInAutomatically: true];
    let _: () = msg_send![&*p, setEnablesRemoteLogin: true];

    let opts = VZMacOSVirtualMachineStartOptions::new();
    let res: Result<(), Retained<NSError>> =
      msg_send![&*opts, setGuestProvisioningOptions: &*p, error: _];
    res.map_err(ns)?;
    Ok(opts)
  }
}

/// `trivm _run <vm>`: starts the VM, macOS's from recoveryOS with `recovery`, and serves
/// `ctl.sock` until the VM stops.
pub fn serve(vm: Vm, recovery: bool) -> io::Result<()> {
  let _lock = vm.lock()?;
  let mtm = MainThreadMarker::new().ok_or_else(|| bad("not on the main thread"))?;
  let app = NSApplication::sharedApplication(mtm);
  app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

  let config = config(&vm)?;
  let name = vm.name.clone();
  let block = RcBlock::new(move |e: *mut NSError| {
    if !e.is_null() {
      eprintln!("trivm: {name} didn't start: {}", message(e));
      process::exit(1);
    }
  });

  // SAFETY: the VM runs on the main queue, which `app.run()` runs.
  let machine =
    unsafe { VZVirtualMachine::initWithConfiguration(VZVirtualMachine::alloc(), &config) };
  match (vm.config.os(), vm.setup, recovery) {
    (Os::Linux, _, true) => return Err(bad(format!("{} runs Linux: no recoveryOS", vm.name))),
    (Os::Macos, _, true) => unsafe {
      let opts = VZMacOSVirtualMachineStartOptions::new();
      opts.setStartUpFromMacOSRecovery(true);
      machine.startWithOptions_completionHandler(&opts, &block)
    },
    (Os::Macos, true, false) if vm.config.provisioned() => {
      let opts = provisioning()?;
      unsafe { machine.startWithOptions_completionHandler(&opts, &block) }
    }
    _ => unsafe { machine.startWithCompletionHandler(&block) },
  }

  let display = Display::new(mtm, &machine, vm.config.screen(), &vm.name);
  STATE.with(|s| *s.borrow_mut() = Some(State { machine, display }));

  let sock = vm.state("ctl.sock");
  let _ = fs::remove_file(&sock);
  let listener = UnixListener::bind(&sock)?;
  let dir = vm.state_dir.clone();
  thread::spawn(move || watch(&dir));
  thread::spawn(move || {
    for conn in listener.incoming().flatten() {
      reply(conn);
    }
  });

  // Keeps the Mac from idle sleep while the VM runs.
  let _ = Command::new("caffeinate").args(["-i", "-w", &process::id().to_string()]).spawn();
  eprintln!("trivm: {} running, pid {}", vm.name, process::id());
  app.run();
  Ok(())
}

/// Ends the process once the VM has stopped, and puts the VM's window away once it is closed.
fn watch(dir: &Path) {
  let mut started = false;
  loop {
    thread::sleep(Duration::from_millis(500));
    let state = main(|s, mtm| {
      s.display.tick(mtm);
      Ok(unsafe { s.machine.state() })
    });

    let Ok(state) = state else { continue };
    match state {
      VZVirtualMachineState::Running | VZVirtualMachineState::Paused => started = true,
      VZVirtualMachineState::Stopped if started => quit(dir, 0, "the VM stopped"),
      VZVirtualMachineState::Error => quit(dir, 1, "the VM stopped on an error"),
      _ => {}
    }
  }
}

fn quit(dir: &Path, code: i32, why: &str) -> ! {
  eprintln!("trivm: {why}");
  let _ = fs::remove_file(dir.join("ctl.sock"));
  process::exit(code)
}

// MARK: Requests

/// A request is its first line and, for `type` and `shot`, the text after it (the text, the
/// shot's path); the reply is `ok [info]` or `err <message>`.
fn reply(mut conn: UnixStream) {
  // Requests are served one at a time: one that never ends mustn't hold up the rest.
  let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
  let mut req = String::new();
  if conn.read_to_string(&mut req).is_err() {
    return;
  }

  let rep = match handle(&req) {
    Ok(x) => format!("ok {x}\n"),
    Err(e) => format!("err {e}\n"),
  };

  let _ = conn.write_all(rep.as_bytes());
}

fn handle(req: &str) -> io::Result<String> {
  let (head, body) = req.split_once('\n').unwrap_or((req, ""));
  let words: Vec<&str> = head.split(' ').collect();
  let num = |x: &str| x.parse::<f64>().map_err(|_| bad(format!("not a number: {x}")));
  let act = |a: Act| main(move |s, _| s.display.act(a));

  match words[..] {
    ["status"] => main(|s, _| {
      let state = unsafe { s.machine.state() };
      let name = match state {
        VZVirtualMachineState::Running => "running",
        VZVirtualMachineState::Starting => "starting",
        VZVirtualMachineState::Stopping => "stopping",
        VZVirtualMachineState::Stopped => "stopped",
        _ => "other",
      };

      Ok(name.to_string())
    }),
    ["stop"] => {
      main(|s, _| unsafe { s.machine.requestStopWithError() }.map_err(ns).map(|_| String::new()))
    }
    ["kill"] => main(|s, _| {
      let block = RcBlock::new(|_: *mut NSError| {});
      unsafe { s.machine.stopWithCompletionHandler(&block) };
      Ok(String::new())
    }),

    ["shot"] | ["shot", "full"] => {
      let (path, full) = (body.to_string(), words.len() == 2);
      // A display just shown needs a moment for its first frame.
      for _ in 0..50 {
        let path = path.clone();
        match main(move |s, _| s.display.shot(Path::new(&path), full)) {
          Ok((w, h)) => return Ok(format!("{w}x{h}")),
          Err(_) => thread::sleep(Duration::from_millis(100)),
        }
      }

      Err(bad("the screen has no frame yet"))
    }

    ["move", x, y] => display::pointer(num(x)?, num(y)?, None, 0, &act),
    ["click", x, y, button, clicks] => {
      let clicks = clicks.parse().map_err(|_| bad("not a click count"))?;
      display::pointer(num(x)?, num(y)?, Some(button == "right"), clicks, &act)
    }
    ["type"] => display::typing(body, &act),
    ["key", keys] => display::keys(keys, &act),
    ["open"] => main(|s, mtm| s.display.open(mtm).map(|_| String::new())),
    ["lit"] => main(|s, _| s.display.lit().map(|x| format!("{x:.2}"))),
    _ => Err(bad(format!("unknown request: {head}"))),
  }
}

/// Sends a request to the VM's process and returns its answer.
pub fn ask(vm: &Vm, req: &str, body: &str) -> io::Result<String> {
  ask_within(vm, req, body, Duration::from_secs(300))
}

/// [ask], waiting for the answer at most `wait`.
pub fn ask_within(vm: &Vm, req: &str, body: &str, wait: Duration) -> io::Result<String> {
  let sock = vm.state("ctl.sock");
  let not_running = || bad(format!("{} isn't running; trivm start {}", vm.name, vm.name));
  if vm.pid().is_none() {
    return Err(not_running());
  }

  let mut s = UnixStream::connect(&sock).map_err(|_| not_running())?;
  s.set_read_timeout(Some(wait))?;
  s.write_all(format!("{req}\n{body}").as_bytes())?;
  s.shutdown(std::net::Shutdown::Write)?;

  let mut rep = String::new();
  s.read_to_string(&mut rep)?;
  match rep.strip_prefix("ok") {
    Some(x) => Ok(x.trim().to_string()),
    None => Err(bad(rep.strip_prefix("err ").unwrap_or(&rep).trim().to_string())),
  }
}
