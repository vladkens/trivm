Done: 2026-11-27

# `desktop` on macOS and Linux

An agent starts a GUI program on the guest's desktop from `exec` the same way on all three systems: `desktop <program> [args]`, which Windows has, starts it in the user's desktop session. nimbo's agent found no documented way on macOS: `launchctl asuser 501 <program>` fails over SSH ("Could not switch to audit session 0x186a2: 1: Operation not permitted"), and `sudo launchctl asuser 501 sudo -u user <program>` shows its window but leaves it a child of the SSH session, which TCC holds to sshd's grants, not to those `os grant` gave the program. Done when, on this Mac, a GUI program started with `desktop` from `exec` shows on a macOS guest's screen with launchd as its parent and its own grants (`AXIsProcessTrusted` true once `os grant` gave it Accessibility, false when started over SSH as it was), and shows on a Linux guest's screen; shots read; the readme and `--help` say how.

## Decisions

- A function defined before `exec`'s command, as Windows' is, not a script put in the guest: it works at once in the VMs made before and in their clones, writes nothing to the guest and needs no check that the guest's copy is trivm's version, as agent-desktop's is. It is there in `exec`'s command line, not in a script that line runs, nor in `trivm ssh` (Windows' interactive PowerShell has it): the help says it is `exec`'s.
- macOS: a launchd job in the user's GUI domain (`launchctl bootstrap gui/<uid>`), as `ui` runs agent-desktop: launchd starts the program, so it is its own responsible process and TCC applies its own grants. Measured on a clone of `macos-27` with a program that shows `AXIsProcessTrusted` in a window, granted Accessibility by `os grant`: started as a job, `launchctl procinfo` says `responsible path = /Users/user/axcheck`, its parent is launchd, and it is trusted; started over SSH in the background, or through `sudo launchctl asuser 501 sudo -u user`, it says `/usr/libexec/sshd-keygen-wrapper` and it isn't. `launchctl asuser` is not it: it needs root, and what it runs stays the SSH session's.
- The job: the program by its path (a name found on `PATH`, as Windows' `Get-Command` finds it), its arguments, the current folder and the caller's `PATH`, which a script's `#!/usr/bin/env` interpreter is found on (launchd's own is `/usr/bin:/bin:/usr/sbin:/sbin`); `ProcessType` Interactive, an app's priority, where launchd's default for a job is a daemon's; `AbandonProcessGroup`, so what the program starts outlives it, as on Windows; its output dropped, as on Windows. The plist is made with `plutil`, which escapes the arguments. `desktop` returns once the job has had a pid for 0.2 s, as Windows' returns once its task runs (a pid may show before the program fails to run); a program that ended before that is reported with its exit code or signal unless 0 (78, `EX_CONFIG`, when launchd can't run it), after 10 s with neither as not started. Both are read from the job's line in the domain's list (`launchctl print gui/<uid>`: pid, last status, label), not from the job's own `launchctl print`, which lists its arguments, and an argument may hold any line. Booting a job out kills its program, so the job stays while the program runs; finished ones (`trivm.desktop.*`, no pid) are booted out at a later `desktop`, once the folder of the call that made each, `<label>` in the user's temporary folder (`getconf DARWIN_USER_TEMP_DIR`, whatever `TMPDIR` says), is gone: a call still reading its job's end isn't robbed of it by another at once.
- An app bundle (`*.app`) goes to `open -a`, as the readme already has it: LaunchServices has launchd start the app, its own responsible process too (TextEdit measured). A running app is brought to the front with no new instance, as a person's double click does.
- Linux gets `desktop` too, so the three systems match: `setsid` starts the program in a session of its own, out of the SSH session's, on sshd's `DISPLAY=:0`, its output dropped. It runs in the background, not with `-f`, which returns before the program is executed: a program that ended within 0.2 s is reported with its exit code unless 0, 127 when it couldn't be executed (a script whose interpreter is missing). The readme's `nohup myapp >/dev/null 2>&1 &` becomes `desktop myapp`. There is no TCC to mind.
- macOS's launchd job is a hack (no API starts a program in a GUI session from SSH): agents.md's list names it, in `ssh.rs` beside `exec`.

## Tasks

- [x] 1. `desktop` on macOS and Linux, defined before `exec`'s command.
- [x] 2. `readme.md`, `--help`, agents.md's hacks.
- [x] 3. The proof on a clone of `macos-27` and of `ubuntu-24.04`.

## Checks

- `make check`, `make test`; a unit test that each system's function runs in its shell (zsh, bash) and refuses a missing program.
- macOS: `exec '<cd> && desktop <program> <args>'`: the window on the screen (a shot read), its arguments and folder in it, its parent launchd, `launchctl procinfo`'s responsible path its own; `AXIsProcessTrusted` true after `os grant <vm> <program> accessibility`, false for the same program started over SSH; a missing program, one that exits 3 and an app bundle.
- Linux: `exec 'desktop <program>'`: the window on the screen, a shot read; a program that can't be executed reported.

## Result

On this Mac (macOS 27.2), on `bd`, a clone of `macos-27` (SIP off), and `bu`, a clone of
`ubuntu-24.04`, in a scratch XDG; the program `axcheck`, a Swift window with
`AXIsProcessTrusted`, its pid, parent, arguments and folder, built on the host and copied in,
granted Accessibility with `os grant bd /Users/user/axcheck accessibility`:

- `exec bd 'cd /tmp && desktop /Users/user/axcheck one "two three"'` returned in 0.5 s: its window
  says trusted, parent 1, `[one] [two three]`, `/private/tmp`; `ps` has launchd as its parent and
  `launchctl procinfo` `responsible path = /Users/user/axcheck` (`macos-desktop-job.png`). Its
  window shows a moment after `desktop` returns.
- The same program started over SSH (`(axcheck &)`), and through `sudo launchctl asuser 501 sudo
  -u user`, nimbo's way: not trusted, `responsible path = /usr/libexec/sshd-keygen-wrapper`
  (`macos-desktop-plain.png`, `macos-desktop-asuser.png`). `launchctl asuser 501` without sudo:
  "Could not switch to audit session 0x186a2: 1: Operation not permitted", as nimbo found.
- `exec bd -- desktop /Users/user/axcheck 'a b' '' 'x&<y>'`: `[a b] [] [x&<y>]`. A script with a
  missing interpreter and an argument with a newline and `\tpid = 5`: "exited: 78, launchd's
  EX_CONFIG when it can't run it", 1. A script run by `#!/usr/bin/env myinterp` with `myinterp`
  on a `PATH` set in the line: started. `desktop` alone: the usage, 2; `desktop nope`: "desktop:
  no program nope", 127; `/bin/sh -c 'kill -SEGV $$'`: "ended on signal 11", 1; a script that
  exits 3: "exited: 3", 1; `false`: "exited: 1", 1; `desktop /System/Applications/TextEdit.app`
  opened TextEdit, its own responsible process, its parent launchd. Finished jobs were gone at the next call; a running one stays, and a
  finished one whose call's folder is there too, till it's gone, from a call with another
  `TMPDIR` too.
- `exec` as before: `sh -c 'exit 7'` exits 7, `printf` with quoted arguments prints them.
- `exec bu 'cd /tmp && desktop xfce4-terminal ... sh "a b" c'`: the terminal on
  the screen, its folder `/tmp`, `[a b] [c]`, its parent 1 and a session of its own
  (`macos-desktop-linux.png`). `desktop xmessage hi` returned in 0.21 s; a script with a missing
  interpreter: "exited: 127", 1, and with `set -e` the line ended there; `desktop false`:
  "exited: 1".

Left: macOS 15 and 26 not checked, as there's no VM of them here.

Shots, in the scratchpad: `macos-desktop-job.png`, `macos-desktop-plain.png`,
`macos-desktop-asuser.png`, `macos-desktop-linux.png`.
