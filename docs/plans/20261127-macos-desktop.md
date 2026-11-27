# `desktop` on macOS and Linux

An agent starts a GUI program on the guest's desktop from `exec` the same way on all three systems: `desktop <program> [args]`, which Windows has, starts it in the user's desktop session. nimbo's agent found no documented way on macOS: `launchctl asuser 501 <program>` fails over SSH ("Could not switch to audit session 0x186a2: 1: Operation not permitted"), and `sudo launchctl asuser 501 sudo -u user <program>` shows its window but leaves it a child of the SSH session, which TCC holds to sshd's grants, not to those `os grant` gave the program. Done when, on this Mac, a GUI program started with `desktop` from `exec` shows on a macOS guest's screen with launchd as its parent and its own grants (`AXIsProcessTrusted` true once `os grant` gave it Accessibility, false when started over SSH as it was), and shows on a Linux guest's screen; shots read; the readme and `--help` say how.

## Decisions

- A function defined before `exec`'s command, as Windows' is, not a script put in the guest: it works at once in the VMs made before and in their clones, writes nothing to the guest and needs no check that the guest's copy is trivm's version, as agent-desktop's is. It is there in `exec`'s command line, not in a script that line runs, nor in `trivm ssh` (Windows' interactive PowerShell has it): the help says it is `exec`'s.
- macOS: a launchd job in the user's GUI domain (`launchctl bootstrap gui/<uid>`), as `ui` runs agent-desktop: launchd starts the program, so it is its own responsible process and TCC applies its own grants. Measured on a clone of `macos-27` with a program that shows `AXIsProcessTrusted` in a window, granted Accessibility by `os grant`: started as a job, `launchctl procinfo` says `responsible path = /Users/user/axcheck`, its parent is launchd, and it is trusted; started over SSH in the background, or through `sudo launchctl asuser 501 sudo -u user`, it says `/usr/libexec/sshd-keygen-wrapper` and it isn't. `launchctl asuser` is not it: it needs root, and what it runs stays the SSH session's.
- The job: the program by its path (a name found on `PATH`, as Windows' `Get-Command` finds it), its arguments, the current folder; `ProcessType` Interactive, an app's priority, where launchd's default for a job is a daemon's; `AbandonProcessGroup`, so what the program starts outlives it, as on Windows; its output dropped, as on Windows. The plist is made with `plutil`, which escapes the arguments. `desktop` returns once the job has a pid; a program that ends within 10 s is reported with its exit code unless 0 (78, `EX_CONFIG`, when launchd can't start it), as Windows' reports a task's result. Booting a job out kills its program, so the job stays while the program runs; finished ones (`trivm.desktop.*`, no pid) are booted out at the next `desktop`.
- An app bundle (`*.app`) goes to `open -a`, as the readme already has it: LaunchServices has launchd start the app, its own responsible process too (TextEdit measured). A running app is brought to the front with no new instance, as a person's double click does.
- Linux gets `desktop` too, so the three systems match: `setsid -f` starts the program out of the SSH session's process group and its terminal, on sshd's `DISPLAY=:0`, its output dropped. The readme's `nohup myapp >/dev/null 2>&1 &` becomes `desktop myapp`. There is no TCC to mind.
- macOS's launchd job is a hack (no API starts a program in a GUI session from SSH): agents.md's list names it, in `ssh.rs` beside `exec`.

## Tasks

- [ ] 1. `desktop` on macOS and Linux, defined before `exec`'s command.
- [ ] 2. `readme.md`, `--help`, agents.md's hacks.
- [ ] 3. The proof on a clone of `macos-27` and of `ubuntu-24.04`.

## Checks

- `make check`, `make test`; a unit test that each system's function runs in its shell (zsh, bash) and refuses a missing program.
- macOS: `exec '<cd> && desktop <program> <args>'`: the window on the screen (a shot read), its arguments and folder in it, its parent launchd, `launchctl procinfo`'s responsible path its own; `AXIsProcessTrusted` true after `os grant <vm> <program> accessibility`, false for the same program started over SSH; a missing program, one that exits 3 and an app bundle.
- Linux: `exec 'desktop <program>'`: the window on the screen, a shot read; `exec` returns at once.
