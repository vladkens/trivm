Done: 2026-11-27

# Retina shots and the guest's screen size

A VM's screen size is its own setting, in points and the display's pixels per point: `trivm create <system> --screen 1152x720@2`, kept as a `screen` key in the VM's config and applied at each start, and `shot --full` takes the display's own pixels, twice a shot's on macOS. Done when, on this Mac, a macOS VM made with `--screen 1152x720@2` gives `shot --full` PNGs of exactly 2304×1440 (by `sips`) with sharp 2x text and `shot`'s of 1152×720, a Linux VM draws and shoots at a size chosen for it, a size changed in the config takes effect at the next start, and clicks at a shot's pixels land where the shot shows them (shots read).

## Decisions

- The size in points with a scale, `<w>x<h>[@<scale>]`, not in pixels: points are a shot's pixels, `click`'s and `ui`'s frames, what agent-desktop reports, and how a Mac's screen is named ("looks like 1152×720"), as Wavo's covers were shot on UTM at 2304×1440. The scale is the system's when left out: 2 on macOS, 1 on Linux.
- macOS takes 1 or 2: VZ's display at the points times the scale in pixels, 220 pixels per inch at 2x (as the 1600×900 default is now), 110 at 1x, and macOS draws a point as that many pixels.
- Linux takes 1 only: its desktop scales itself, by its own settings (GNOME's, KDE's, Xfce's), which VZ's virtio-gpu doesn't carry; the size is the scanout's, the desktop's preferred mode. Its width is a multiple of 8: the kernel makes the mode by CVT, which rounds it down (1366x768 drew 1360×768, found by the review and seen on Ubuntu).
- Windows is refused for now: on main it draws on ramfb at 800×600; its size comes with virtio-gpu (#10), as QEMU's `xres` and `yres` from the same key, once both are in.
- The `screen` key in `<vm>.conf`, written by `create --screen`; without it the system's default, as VMs made before have. Read at each start, as `cpus` and `memory` are: edit it and restart. A clone keeps it. From 640×480 points up to 7680×4320 pixels.
- `shot --full`: the display's own pixels; `shot` stays a pixel per point. 2x shots by default would make each macOS shot four times the pixels (3200×1800 now) for the agents that read them, and move `click`'s and `ui`'s coordinates, which stay a shot's pixels; a full shot's are the scale times those.

## Tasks

- [x] 1. The `screen` key and `create --screen`: parsed, checked, written; VZ's display from it at each start, on macOS and Linux; Windows refused.
- [x] 2. `shot --full`.
- [ ] 3. The proof on macOS and Linux; `readme.md`, `--help`. Linux and the docs done; macOS left (Result).

## Checks

- `make check`, `make test`; unit tests for the size's parsing and the config's key.
- Task 3 on this Mac: sizes by `sips`, shots read, clicks on what they show.

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
- Refused before anything is made: `--screen 1152x720@3` on macOS, `1366x768` on Linux, any size on
  Windows (ramfb's 800×600).

Left: the macOS proof. A fresh macOS install was ruled out for this Mac's disk, and both a clone
of the person's `macos-27` and a copy of it in this run's own folders were refused by this
session's permissions, so no macOS VM has run this branch: `1152x720@2`'s 2304×1440 at 220 ppi, its `shot --full` and the 1x mode at 110
ppi are unchecked. A VM without `screen` gets the display it had, 3200×1800 at 220 ppi. Windows'
size waits for virtio-gpu (#10), and the roadmap's item is in #10's roadmap, to be ticked once
both are in.
