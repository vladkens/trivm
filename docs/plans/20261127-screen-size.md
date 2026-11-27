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

- [ ] 1. The `screen` key and `create --screen`: parsed, checked, written; VZ's display from it at each start, on macOS and Linux; Windows refused.
- [ ] 2. `shot --full`.
- [ ] 3. The proof on macOS and Linux; `readme.md`, `--help`.

## Checks

- `make check`, `make test`; unit tests for the size's parsing and the config's key.
- Task 3 on this Mac: sizes by `sips`, shots read, clicks on what they show.
