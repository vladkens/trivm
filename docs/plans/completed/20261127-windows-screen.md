Done: 2026-11-27

# Windows' screen at any size

A Windows VM's screen is 1920×1080, as a Linux VM's is, not ramfb's 800×600: the VM's display is QEMU's virtio-gpu, drawn by virtio-win's `viogpudo`, which trivm gives Windows from the virtio-win ISO it already downloads for NetKVM. Done when, on this Mac, a fresh `trivm create windows-11` installs with no clicks to a desktop of 1920×1080, `shot` takes it at that size, `click`, `type` and `key` land where the shot's pixels say, `open`'s VNC serves it, and a VM made before the change still starts and draws as it did (shots read).

## Decisions

- The driver: `viogpudo/w11/ARM64` of virtio-win 0.1.302, the ISO trivm pins for NetKVM: `viogpudo.inf` (100.103.104.30200, 2026-07-22) matches `PCI\VEN_1AF4&DEV_1050`, QEMU's `virtio-gpu-pci`, and its catalog is signed by Microsoft Windows Hardware Compatibility Publisher, as NetKVM's is, so Windows loads it with driver signing enforced (`pnputil /enum-drivers` in the VM: that signer). Only its `.inf`, `.sys` and `.cat` go in the seed (130 KB); not the resolution service (`vgpusrv.exe`), which follows a host window's size, and trivm has no such window.
- Added by Setup, from the seed's `$WinPEDriver$\viogpudo`, the folder Setup looks for at the root of every drive: no drive letter to guess, as `DriverPaths` in the windowsPE or offlineServicing pass would need. Setup loads it in Windows PE (its log: `Succeeded in loading driver from path [E:\$WinPEDriver$\viogpudo\viogpudo.inf]`) and injects it into the system it installs (`Successfully injected driver package in the new OS`); Windows installs it for virtio-gpu when it first finds one, as it starts. Not `pnputil` in the first logon's script, as NetKVM is: the same, a step later.
- The device: `virtio-gpu-pci,xres=1920,yres=1080`. `xres` and `yres` are the mode its EDID prefers (QEMU's default 1280×800), the one `viogpudo` offers with no window on the host to follow. Windows picks 100% scaling for it (`AppliedDPI` 96).
- ramfb stays for Setup, while its media is attached, and the VM has one display at a time. Windows PE draws nothing on virtio-gpu: tried first, virtio-gpu alone from the start showed "Display output is not active" through Windows PE's whole 15 minutes, though Setup's log has the driver loaded, as edk2's virtio-gpu driver (VirtioGpuDxe, which draws edk2's own screens) leaves Windows no framebuffer; an install failing there would show nothing, and SSH answers only at its end, so `create` would wait 90 minutes blind. With ramfb beside virtio-gpu, Windows has two monitors, the tablet's absolute axes span both and a shot is one of them. So Setup runs on ramfb, as before, and `create` restarts the VM once it is set up: the desktop up after the first sign-in, `stop`, then started without the media, the seed and ramfb, on virtio-gpu (about a minute more). The restart replaces the CDs' `device_del`.
- One start sets a VM up: a second `start` meanwhile would restart it too, and the two race (the review had one fail, "windows-11 is in use"). `start` takes a lock on the `setup` mark first; a second waits on it, saying so, then finds the VM set up and starts as for one that is. After the wait the lock is checked to be on the VM's own mark: a VM deleted meanwhile and made again of the name has a mark of its own, which `start` waits on in turn. `delete` refuses a VM a start sets up ("is in use: a start sets it up"), as one being made: between the restart's two processes the VM is unheld, and a VM deleted and made again there would get the old setup's restart.
- A VM made before has no `viogpudo`: with virtio-gpu it would show nothing. `create` writes the mark `gpu` in a new Windows VM's data folder, as `setup` says what its disk holds, and `start` gives a VM virtio-gpu with it and no media attached, ramfb otherwise, so an older VM starts as it did, at 800×600. Clones copy the mark with the disk.
- `shot`, `click`, `type`, `key` and `open` change nothing: QMP's `screendump` and QEMU's VNC take the first console, the only one; the tablet's axes span the screen's size, read from its dump as before.

## Tasks

- [x] 1. `viogpudo` in the seed's `$WinPEDriver$`, the `gpu` mark, `virtio-gpu-pci` for a set-up VM with it, ramfb for Setup and for one without; the restart after Setup, one start at a time; unit tests; `readme.md`.
- [x] 2. The proof: a fresh `create windows-11` at 1920×1080 with no clicks; `shot`, `click`, `type`, `key`, `open`'s VNC; `stop` and `start`; a VM without the mark on ramfb.
- [x] 3. The roadmap.

## Checks

- `make check`, `make test`; unit tests for QEMU's command line with and without the mark and with the media, the mark read with a VM, and the setup's lock.
- Task 2 on this Mac, with the shots read.

## Result

On this Mac (QEMU 11.1.3, swtpm 0.10.2 from Homebrew), in folders of its own, the media from the
cache, shots read:

- `trivm create windows-11`: installed with no clicks to the desktop in 25 minutes (15 before on
  this Mac; the other lane's VMs ran beside it, a load average of 37). Shots every 30 s: Setup on
  ramfb at 800×600, "Installing Windows 11 16%" (`windows-screen-setup-pe.png`), the first sign-in
  running the setup script (`windows-screen-setup-logon.png`); "restarting windows-11 on
  virtio-gpu"; "Welcome" at 1920×1080 (`windows-screen-setup-59.png`), the desktop
  (`windows-screen-1.png`). In the VM: one display adapter, "Red Hat VirtIO GPU DOD controller",
  status OK, at 1920×1080; viogpudo 100.103.104.30200 signed by Microsoft Windows Hardware
  Compatibility Publisher; 100% scaling.
- `shot`: 1920×1080 PNGs. `click 716 1056` opened Start, the pointer on its button
  (`windows-screen-2-start.png`); `type notepad`, `key enter` opened Notepad, `type` and `key enter`
  wrote two lines into it (`windows-screen-3-notepad.png`); `click 416 172` closed Notepad's tip
  and `click 106 85` opened its Edit menu (`windows-screen-4-edit.png`).
- `open`'s VNC: after QMP's `set_password` and `query-vnc`, as `open` does, an RFB handshake with
  VNC authentication (`1234`) got a framebuffer of 1920×1080, "QEMU (windows-11)"; a wrong
  password was refused. Screen Sharing itself wasn't opened: nothing goes on this Mac's own
  desktop in these runs.
- `stop` in 14 s, `start` to the desktop in 60 s, at 1920×1080 (`windows-screen-5-restart.png`).
- Two starts at once of a VM marked as being set up: the first started it with the media and
  ramfb, waited for the desktop, restarted it on virtio-gpu; the second said "waiting for another
  start setting windows-11 up", then "windows-11 is up" (57 s), both exiting 0, the VM at
  1920×1080 (`windows-screen-7-two-starts.png`).
- `delete` while a start set the VM up: "windows-11 is in use: a start sets it up", the VM
  left running; the start went on to the desktop on virtio-gpu.
- A VM without the mark: a clone with `gpu` removed started on ramfb in 29 s, at 800×600, Basic
  Display's (`windows-screen-6-ramfb.png`). Deleted after.

The first try, virtio-gpu from the start, installed too (22 minutes, the same desktop) but showed
nothing in Windows PE; the branch's review asked for Setup's screens back, hence ramfb for Setup
and the restart. Left: the screen's size is fixed at 1920×1080, which the next roadmap item makes
a VM's choice.
