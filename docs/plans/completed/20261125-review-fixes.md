Done: 2026-11-26

# The stack-wide review's fixes

The fixes from the review of the whole stack (trivm#1 to trivm#6, pr-gate) that main still needs,
on main's code: this branch first joined the `ui` and `older` lanes and made macOS 13 and 14 with
no clicks; main has since taken both lanes (#5, #6) with fixes of its own and dropped 13 and 14.
Done when `git diff main` holds only those fixes, each checked against main, and each fix that
changes what trivm does is shown on this Mac: `macos-27` started, a command, stopped, and stopped
with its SSH off; a Linux VM; a `run` clone; Windows for its fix.

## Decisions

- Main merged in with no rebase: `ui`, then main's squash of it (387fca9) with `-s ours`; `older`,
  then main's squash of it (d048ae4) with `-s ours`. Where this branch and a lane had both changed
  the code (13 files, rewritten on both sides), the lane's code was taken whole, so the tree was
  main's and this plan; each fix main still needs was then made again on main's code, a commit
  each.
- Dropped with macOS 13 and 14, which #6 removed: the Setup Assistant answered from shots
  (`assistant.rs`, `vision.rs`; `permissions.rs`'s `find_text` stays the one Vision reader),
  Remote Login turned on through Finder's Terminal, and their parts of this plan.
- Kept, as main still has the bug:
  - `stop` of a guest whose SSH doesn't answer: main sends SSH's shutdown only to a guest with an
    address, kills one with none at once, and kills one whose SSH is off after 2 minutes. Now a
    connection to its SSH port within 2 s decides: SSH's shutdown when it answers, else the
    power button (VZ's request within 5 s), and on macOS the Shut Down it asks clicked, read
    with Vision. Windows gets both, as before: its forwarded port answers before its sshd does.
  - Shut Down read up to 30 times, not for 30 s, found in this proof: Vision's first read in a
    newly built or moved binary took 27 to 28 s here (0.1 s after), which used up the 30 s
    before the dialog was read, and `stop` killed the VM after 2.5 minutes.
  - The pointer mapped at the display's size now, the IOSurface's: main maps a point by the size
    the VM started with, so after a resolution change in the guest a click lands scaled, and one
    past the old width or height is refused.
  - `click --name` refuses only a point left of or above the screen: main checks the element's
    middle against the size the VM started with.
  - `click --name`: a match and the matches inside it of its name are one element. GTK 4 names a
    file's cell or row, a cell in it, its icon and its label alike (Debian's Files), where main
    says "3 elements named" and asks for a frame.
  - `run`'s clone killed by its pid: main checks the clone's data folder, then stops the VM by its
    name, which may be another VM's by then. Now the `pid` file is opened before the folder is
    checked, and the process holding its lock gets SIGKILL by its pid.
  - Windows: sshd takes keys only (`PasswordAuthentication no`, `KbdInteractiveAuthentication
    no` put first in its config). Main's sshd takes the password `1234` on a port any user of
    the Mac reaches.
- Dropped as main already has them:
  - The pid file written in one write and read again: main writes it in one write once locked,
    and a read before it finds it empty, which counts as not running.
  - A gone holder's pid skipped, and SIGKILL only to a VM's process: main reads a pid only while
    its writer holds the lock (files close on exec), and QEMU writes its own before it execs.
  - `stop` following a starting Windows VM to QEMU's pid, and requests to the VM process
    bounded to 5 s: #3's takeover and `ask_within`.
  - QEMU's log opened before swtpm starts; `scp://` paths percent-encoded; the SSH key made
    beside its place and renamed in whole: each in main as this branch had it.
  - `xprop` and `xwininfo` on each Linux: #6's `x11-utils` and Fedora's `xwininfo`.
- Dropped for main's version of the same fix: Windows `exec`'s line bounded (main's `CMD_MAX`,
  its error saying to copy a script) and `desktop` defined only for a script that calls it (main
  defines it in every prelude).
- Dropped: the style pass, as the code it touched is main's now.

## Tasks

- [x] 1. `ui` and main's squash of it merged, then `older` and main's squash of it.
- [x] 2. macOS 13 and 14's code and plan dropped.
- [x] 3. Each review fix checked against main: kept where main has the bug, made on main's code.
- [x] 4. The proof on this Mac, shots read.

## Checks

- `make check`, `make test`; unit tests for `click --name`'s nested names and `run`'s discard by
  pid (a process holding the lock killed, another VM's folder left alone).
- Task 4 on this Mac: the systems each fix touches, with a scratch VM where the fix needs one.

## Result

Run on this Mac with this branch's binary, shots read:

- `macos-27`: started in 17 s, `sw_vers` 27.2, a shot, stopped through SSH in 8 s. A clone of it
  with sshd unloaded: `stop` pressed the power button, read Shut Down and clicked it, and the VM
  shut down in 41 s, 28 of them Vision's first read in a new binary. Before the 30-reads fix two
  of three such `stop`s killed it after 2 minutes 33 seconds.
- `debian-13`, a scratch VM: the pointer read back with XQueryPointer where `move` put it at
  1920×1080, 2560×1600 and 1600×1200; at 2560×1600 with Files maximized, `click --name
  Templates --double` opened the folder (a cell, a cell and a label of that name) and `click
  --name "List View"` at x 2479 switched the view; with its sshd stopped, `stop` by the power
  button in 2 s.
- `run debian-13` ran its command in `debian-13-run-1`, gone after; a `run` killed with SIGKILL
  left its clone to the reaper, which killed the VM process by its pid and removed the clone.
- `windows-11`, a scratch VM: sshd's config begins with both lines, and its SSH offers only
  `publickey` to a client asking for a password; stopped in 14 s, then deleted.

Shots: `pr7-macos-27.png`, `pr7-macos-27-stop-dialog.png`, `pr7-macos-27-stop-down.png`,
`pr7-debian-13-{1920x1080,2560x1600,1600x1200}.png`, `pr7-debian-13-templates.png`,
`pr7-debian-13-listview.png`, `pr7-run-debian-13.png`, `pr7-windows-11.png`, in the scratchpad.
