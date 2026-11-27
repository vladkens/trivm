Done: 2026-11-27

# Retina shots and the guest's screen size

A VM's screen size is its own setting, in points and the display's pixels per point: `trivm create <system> --screen 1152x720@2`, kept as a `screen` key in the VM's config and applied at each start, and `shot --full` takes the display's own pixels, twice a shot's on macOS. Done when, on this Mac, a macOS VM made with `--screen 1152x720@2` gives `shot --full` PNGs of exactly 2304×1440 (by `sips`) with sharp 2x text and `shot`'s of 1152×720, a Linux VM and a Windows VM draw and shoot at a size chosen for them, a size changed in the config takes effect at the next start, and clicks at a shot's pixels land where the shot shows them (shots read).

## Decisions

- The size in points with a scale, `<w>x<h>[@<scale>]`, not in pixels: points are a shot's pixels, `click`'s and `ui`'s frames, what agent-desktop reports, and how a Mac's screen is named ("looks like 1152×720"), as Wavo's covers were shot on UTM at 2304×1440. The scale is the system's when left out: 2 on macOS, 1 on Linux and Windows.
- macOS takes 1 or 2: VZ's display at the points times the scale in pixels, 220 pixels per inch at 2x (as the 1600×900 default is now), 110 at 1x, and macOS draws a point as that many pixels.
- Linux takes 1 only: its desktop scales itself, by its own settings (GNOME's, KDE's, Xfce's), which VZ's virtio-gpu doesn't carry; the size is the scanout's, the desktop's preferred mode. Its width is a multiple of 8: the kernel makes the mode by CVT, which rounds it down (1366x768 drew 1360×768, found by the review and seen on Ubuntu).
- Windows takes the size as QEMU's virtio-gpu `xres` and `yres` (#10's device, 1920×1080 by default): the scanout's size, which virtio-win's `viogpudo` offers as a mode of its own beside its 640×480, 1024×768 and the EDID's, and the mode QEMU's EDID prefers. Any width: `viogpudo` keeps a line at 4 bytes a pixel and rounds nothing (`SetCustomDisplay`, `SetVideoModeInfo`), and 1366x768 drew 1366×768, so the multiple of 8 is Linux's only.
- `start` sets the mode once the desktop is up when Windows draws another, and says so ("Windows drew 1920x1440, set to 1600x900"): `ChangeDisplaySettingsExW` with `CDS_UPDATEREGISTRY`, which keeps it for the monitor, in the desktop's session through a scheduled task, as `desktop` runs a program. Windows took another twice. It keeps a monitor's mode under an id holding one byte of the EDID, its checksum (`RHT12340_2A_07DE_33`), which 1600×900 and 2304×1440 share: 2304×1440 drew 1400×1050, Windows having kept 1600×900 for the id. And past a pixel clock of 655 MHz at QEMU's 75 Hz (about 6.2 megapixels: 3840×1650 and up) or 4096 a side, QEMU's EDID names the mode only in a DisplayID block, which `viogpudo` doesn't pass on (it gives Windows the first 256 bytes): 1024×768. Only a start that runs QEMU checks, as a running VM may have another size; it costs one SSH command.
- Windows takes up to 5120×2880: at 7680×4320 Windows set the mode, but `viogpudo` showed nothing ("Display output is not active"), as with QEMU's `max_hostmem` at 1 GB; it keeps the frame in non-paged pool, 133 MB there. 5120×2880 it drew.
- Windows takes 1 only; `@2` is refused, not made Windows' 200%. Windows' scaling is its own, as a Linux desktop's: Windows picks it from the EDID by its own rules, 100% at 1366×768, 1600×900, 2560×1440 and 3840×1600 but 250% at 3200×1800 (`AppliedDPI`), and the user changes it in Settings; trivm's points there are the display's pixels. Making `@2` 200% would mean writing the user's per-monitor setting, an offset from the scale Windows recommends, and signing in again at each start. And it isn't macOS's 2x: an app that isn't DPI-aware is drawn at 100% and stretched, and agent-desktop's frames would need the scale.
- A Windows VM made before #10 has no `viogpudo` and draws on ramfb at 800×600: one with a `screen` is refused at `start`, while set up too, and starts as before without it. A new Windows VM takes it at `create`, as it gets the driver.
- The `screen` key in `<vm>.conf`, written by `create --screen`; without it the system's default, as VMs made before have. Read at each start, as `cpus` and `memory` are: edit it and restart. A clone keeps it. From 640×480 points up to 7680×4320 pixels.
- `shot --full`: the display's own pixels; `shot` stays a pixel per point. 2x shots by default would make each macOS shot four times the pixels (3200×1800 now) for the agents that read them, and move `click`'s and `ui`'s coordinates, which stay a shot's pixels; a full shot's are the scale times those.

## Tasks

- [x] 1. The `screen` key and `create --screen`: parsed, checked, written; VZ's display from it at each start, on macOS and Linux.
- [x] 2. `shot --full`.
- [ ] 3. The proof on macOS and Linux; `readme.md`, `--help`. Linux and the docs done; macOS left (Result).
- [x] 4. Windows, once #10 is in: virtio-gpu's `xres` and `yres` from the key at each start, the mode set when Windows draws another, `@2` refused, up to 5120×2880, a VM on ramfb refused; `--help`; the proof on a fresh `windows-11`.

## Checks

- `make check`, `make test`; unit tests for the size's parsing and the config's key, QEMU's command line at a size, a VM on ramfb refused, and the script that sets Windows' mode.
- Tasks 3 and 4 on this Mac: sizes by `sips`, shots read, clicks on what they show.

## Result

On this Mac, in folders of its own, shots read:

- `trivm create ubuntu-24.04 --screen 1280x800` (Xfce): the config got `screen = 1280x800@1`; the
  desktop came up at 1280×800, `xrandr`'s mode and both `shot` and `shot --full` 1280×800 by `sips`
  (`screen-size-linux-1.png`). `click 70 400 --double` put X's pointer at 70,400 (`xdotool`) and
  opened Home (`screen-size-linux-2-home.png`); `ui` framed Thunar's places where the shot has
  them, and `click --name Desktop` opened that folder (`screen-size-linux-3-desktop.png`).
- `screen` edited to 1366x768, then restarted: the desktop came up at 1360×768
  (`screen-size-linux-4-1366.png`), as the review said, hence widths of a multiple of 8 on Linux;
  `create --screen 1366x768` now says so. Edited to 1440x900 and restarted: 1440×900 by `xrandr`
  and `sips` (`screen-size-linux-5-1440.png`); `click 647 877` on the dock's terminal put the
  pointer at 647,877 and opened it (`screen-size-linux-6-terminal.png`). Deleted after.
- Refused before anything is made: `--screen 1152x720@3` on macOS, `1366x768` on Linux.
- `trivm create windows-11 --screen 1366x768 --memory 4`: installed with no clicks in 13 minutes,
  restarted on virtio-gpu, up in 23 s more; the config got `screen = 1366x768@1`, QEMU
  `virtio-gpu-pci,xres=1366,yres=768`. The desktop came up at 1366×768, Windows' mode (Red Hat
  VirtIO GPU DOD controller) and both `shot` and `shot --full` 1366×768 by `sips`, at 100%
  (`screen-size-win-1.png`). `click 795 744` on the taskbar's File Explorer opened it, the pointer
  on the icon (`screen-size-win-2-explorer.png`); `click 201 483` opened Music
  (`screen-size-win-3-music.png`).
- `screen` edited to 1600x900, then `stop` and `start` (17 s): 1600×900 by Windows and `sips`
  (`screen-size-win-4-1600.png`); `click 556 876` opened Start, `click 1040 375` Calculator, and
  clicks on 7, ×, 6 and = gave 42 (`screen-size-win-5-start.png`, `screen-size-win-7-42.png`).
- Edited and restarted again: 2560x1440, 3200x1800 and 3840x1600 drew at their sizes
  (`screen-size-win-13-2560x1440.png`, `screen-size-win-11-3200.png`,
  `screen-size-win-9-3840x1600.png`); 3840x1650 and 3840x2160 drew 1024×768, and 2304x1440
  1400×1050, hence the mode set at start. With it: "Windows drew 1400x1050, set to 2304x1440",
  and 2304×1440, 3840×2160 and 5120×2880 by `sips` (`screen-size-win-16-2304x1440.png`,
  `screen-size-win-16-3840x2160.png`, `screen-size-win-17-5120x2880.png`); 7680x4320 showed
  "Display output is not active" (`screen-size-win-16b.png`), hence 5120×2880 at most.
- The branch's last build, on the same VM: 1366x768 again, File Explorer opened by
  `click 795 744` (`screen-size-win-21-1366-explorer.png`); edited to 1600x900 and restarted:
  "windows-11: Windows drew 1920x1440, set to 1600x900", 1600×900 by `sips`, and Calculator's
  7 × 6 = 42 by clicks at the shot's pixels (`screen-size-win-23-1600-42.png`); the key removed:
  1920×1080 (`screen-size-win-24-default.png`). Deleted after.
- Refused before anything is made: `--screen 1280x800@2` on Windows ("Windows draws at 1x here;
  its scaling is the user's own"), `7680x4320` and `5128x2880` ("Windows draws up to 5120x2880
  here").

Left: the macOS proof. A fresh macOS install was ruled out for this Mac's disk, and both a clone
of the person's `macos-27` and a copy of it in this run's own folders were refused by this
session's permissions, so no macOS VM has run this branch: `1152x720@2`'s 2304×1440 at 220 ppi, its `shot --full` and the 1x mode at 110
ppi are unchecked. A VM without `screen` gets the display it had, 3200×1800 at 220 ppi. The
roadmap's item stays open until then.
