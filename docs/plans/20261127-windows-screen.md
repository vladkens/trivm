# Windows' screen at any size

A Windows VM's screen is 1920×1080, as a Linux VM's is, not ramfb's 800×600: the VM's display is QEMU's virtio-gpu, drawn by virtio-win's `viogpudo`, which trivm gives Windows from the virtio-win ISO it already downloads for NetKVM. Done when, on this Mac, a fresh `trivm create windows-11` installs with no clicks to a desktop of 1920×1080, `shot` takes it at that size, `click`, `type` and `key` land where the shot's pixels say, `open` shows it in Screen Sharing, and a VM made before the change still starts and draws as it did (shots read).

## Decisions

- The driver: `viogpudo/w11/ARM64` of virtio-win 0.1.302, the ISO trivm pins for NetKVM: `viogpudo.inf` (100.103.104.30200, 2026-07-22) matches `PCI\VEN_1AF4&DEV_1050`, QEMU's `virtio-gpu-pci`, and its catalog is signed by Microsoft Windows Hardware Compatibility Publisher, as NetKVM's is, so Windows loads it with driver signing enforced. Only its `.inf`, `.sys` and `.cat` go in the seed (133 KB); not the resolution service (`vgpusrv.exe`), which follows a host window's size, and trivm's has none.
- Installed by Setup, not by the first logon's script: the seed holds it in `$WinPEDriver$\viogpudo`, the folder Windows Setup looks for at the root of every drive. Setup loads it in Windows PE, so Setup's own screens show on virtio-gpu once it has started, and adds it to the installed system's driver store, so Windows draws at 1920×1080 from its first boot. No drive letter to guess, as the windowsPE pass's `DriverPaths` and the offlineServicing pass's would need; `pnputil` in the first logon's script would leave Setup and OOBE with no screen at all.
- The device: `virtio-gpu-pci,xres=1920,yres=1080`. `xres` and `yres` are the size its EDID gives as the display's preferred mode (QEMU's default 1280×800), which `viogpudo` takes; with no window on the host, nothing asks for another.
- No ramfb on a new VM. edk2 draws its own screens on virtio-gpu (its VirtioGpuDxe); from Windows' boot manager until the driver loads the screen says "Display output is not active", as edk2's virtio-gpu driver gives no framebuffer for Windows to keep drawing on, a few seconds at each start and Windows PE's first half minute while it is set up. Two display devices would be two monitors in Windows, the pointer's absolute axes across both, and the shot of one.
- A VM made before has no `viogpudo`: with virtio-gpu it would show nothing. `create` writes the mark `gpu` in a new Windows VM's data folder, as `setup` says what its disk holds; a VM without it keeps ramfb and its 800×600. Clones copy the mark with the disk.
- `shot`, `click`, `type`, `key` and `open` change nothing: QMP's `screendump` and QEMU's VNC take the first console, the only one; the tablet's axes span its size, read from the screen's dump as before.

## Tasks

- [ ] 1. `viogpudo` in the seed's `$WinPEDriver$`, the `gpu` mark, `virtio-gpu-pci` for a VM with it, ramfb for one without; unit tests.
- [ ] 2. The proof: a fresh `create windows-11` at 1920×1080 with no clicks; `shot`, `click`, `type`, `key`, `open`; `stop` and `start`; a VM without the mark on ramfb.
- [ ] 3. `readme.md`, the roadmap.

## Checks

- `make check`, `make test`; unit tests for QEMU's command line with and without the mark, and the seed's files.
- Task 2 on this Mac, with the shots read.
