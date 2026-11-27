Done: 2026-11-27

# No screen saver or lock on macOS VMs

`MAC_TUNE` is meant to turn sleep, the screen saver and the lock off, yet a macOS 27 VM left idle
for about 20 minutes shows the screen saver and then asks for the password, which stops long agent
runs that only read shots. Find which setting doesn't stick and why, fix it for new VMs and for the
ones made already, and say how the existing ones get it. Done when a clone of a fixed VM, left idle
longer than the old 20 minutes, shows its desktop with no screen saver and no lock, in a shot read.

## Decisions

- The cause, found on clones of `macos-27`:
  - The screen saver's `idleTime` is written with `-currentHost`, in a file named for the
    machine's IOPlatformUUID. A clone gets a new machine identifier, so a new UUID, and so none:
    macOS's default, 20 minutes. macOS reads it at login only: written in a running session it
    does nothing until the next, so even a new VM's first session had the default.
  - `sysadminctl -screenLock off` under sudo exits 0 and says the lock is off, but the user's
    stays on: it is the user's own setting, kept in their keybag for this host
    (`~/Library/Keychains/<UUID>/user.kb`), so a clone has it on again too. Run as the user it
    takes at once.
  - Sleep (`pmset`) stays off in a clone.
- The screen saver's time written for any host (`defaults write` without `-currentHost`), which
  macOS reads when there is none for the host and which a clone keeps, and for this host too, as a
  nonzero one there (System Settings') would win.
- A new VM has it from its first session: 15 and 26 by the setup script, before the first login;
  27, whose user VZ's provisioning makes and logs in with no step before, by one restart at the
  end of its setup, at the boot that made the user only (a second `start` meanwhile runs the setup
  too). The restart is told by `kern.boottime`, the shutdown run in the background so a stalled
  one times out.
- Existing VMs get it at `start`, not a one-off command: each macOS start writes the time (read
  from the next login) and turns the lock off as the user (at once). A VM made before this has the
  screen saver off from its next boot after a start, and so does every clone of it made after that
  start, from its first boot; its lock is off from that start on. One SSH call at start, no wait.
- The lock's failure doesn't fail `start`, as before: a VM whose password was changed still starts.

## Tasks

- [x] 1. The setup script writes the screen saver's time for any host; `MAC_TUNE` loses its
  `-currentHost` line and its sudo lock line; macOS 27 restarts once after its setup.
- [x] 2. `start` turns both off on macOS, as the user.
- [x] 3. The proof: a clone of a fixed VM idle over 20 minutes, a shot read; the lock checked.

## Checks

- `make check`, `make test`.
- On a clone of `macos-27`: the old state (screen saver at the default, lock on), then the fix,
  then a real idle wait of 25 minutes on a fresh clone; shots read.

## Result

Run on this Mac on clones of `macos-27`, shots read:

- The bug, on a clone: no per-host `idleTime` for its UUID, `sysadminctl` saying the lock is
  immediate; the screen saver after idle and the lock screen asking for the password.
- What macOS reads: a per-host or any-host `idleTime` of 60 written before a boot started the
  screen saver after a minute; written in a running session, neither did. The lock: off under
  sudo, the screen saver still locked; off as the user, it woke to the desktop, at once
  (immediate as the user locked again, off unlocked).
- The fix: a clone started with this branch, then a clone of it (new UUID) started: any-host
  `idleTime` 0 kept, the lock off at its first start; left idle 1523 s (25 minutes), its desktop
  with no screen saver and no lock; the screen saver started by hand woke with no password.
- The setup path run on that running clone (its `setup` mark set): the key, the time zone from
  UTC back to the host's, both `idleTime`s 0, no restart (the user older than the boot); the
  restart as `macos_setup` does it: back on a new `kern.boottime` in 19 s, the VM process kept.
- Left: a new macOS 27 VM through `create`, for its restart, not run here: no disk room for one.
