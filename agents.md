# trivm

A command line that installs and runs macOS, Linux and Windows virtual machines on an Apple silicon
Mac, so a coding agent can test desktop apps on other systems from the terminal: a system installed
with no clicks, started in the background, commands and files through SSH, screenshots, clicks and
typing; a person opens a window onto a running VM to look or use the mouse. Agents carry out each
change end to end by following this file.

Each fact has one source, which this file points to: what trivm does is `readme.md` and
`trivm --help`, dependencies `Cargo.toml`, checks the `Makefile`, what comes next
`docs/roadmap.md`, the work under way `docs/plans/`.

## Scope

- Host: macOS on Apple silicon. Guests are arm64 only, never emulated: macOS and Linux on
  Virtualization.framework, Windows 11 on QEMU with HVF (Homebrew's `qemu` and `swtpm`), run as a
  child process and driven over QMP; its install media made with Homebrew's `wimlib`.
- trivm drives no other app: no AppleScript, no UTM, no permission prompts on the host. A
  Virtualization.framework VM runs in trivm's own process. Screenshots and input go through the
  VM's display from the host (the VM's view, QMP for QEMU), commands and files through SSH, so a
  guest needs no agent of ours.
- Systems come from their own sources (Apple's IPSW, the distros' cloud images, the ESD of
  Microsoft's Media Creation Tool) and are installed by code; trivm hosts nothing.
- Pure Rust: Virtualization.framework and AppKit through the `objc2-*` crates; no Swift, no C
  compiled, no bindings generated. Enable only the `objc2-*` features the code uses.
- Prefer a small own routine to a dependency. Change dependencies only with `cargo add` /
  `cargo remove` / `cargo upgrade`, never by hand.

## Code

- Start every `.rs` file with the line
  `// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm`, then a `//!` doc
  whose first sentence says what the file does.
- Comment only what the code can't say: platform quirks, measured numbers, ordering that matters.
  Write code that reads like the code around it. Code taken from another project names it in the
  file's header (project, licence, link to the file).
- Helper scripts go in `scripts/`, kebab-case, Python run with `uv run`.

## Hacks

trivm works around what Apple and Microsoft give no API for. A hack is fine; keep it apart from
what trivm does, so the two aren't mixed up:

- A hack that knows nothing of VMs, such as a disk format's code, is a file of its own, as an
  outside crate would be: it uses nothing of trivm's but `bad`, and its header says what trivm
  needs it for. `apfs.rs` (a file's owner set to root on a macOS disk, for 15's and 26's first
  boot), `qcow2.rs` (the distros' disks to raw for VZ), `dbus.rs` (D-Bus's wire protocol, for
  QEMU's microphone).
- A hack inside a command stays in that command's file, its doc comment saying which API is
  missing: the VM's view in a hidden window for shots and input (`display.rs`); TCC's database
  written and recoveryOS read with Vision (`permissions.rs`); macOS's Shut Down at its power
  button read with Vision and clicked (`main.rs`'s `stop`); `/etc/kcpassword` written for the
  automatic login (`system.rs`); agent-desktop as a launchd job for its grant (`ui.rs`);
  `desktop`'s launchd job on macOS (`ssh.rs`); `LabConfig`'s skipped checks and `desktop`'s
  scheduled task (`windows.rs`).
- A hack goes with the last system that needs it: macOS 27 makes its user itself, so `apfs.rs`
  goes with 15 and 26. Keep this list current in the commit that adds or removes one.

## Tests

- `make check` (format; clippy with warnings denied and rust-lint's rules, on the lines a branch
  changes) and `make test` run before every commit.
- Unit-test what breaks silently: QEMU's command line, the unattended-install files, a VM's
  configuration. The proof of a change to how a system is installed or driven is that system on
  this Mac: installed, started, a command run in it, a screenshot taken.
- A scratch run of trivm, beside this Mac's VMs, sets `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and
  `XDG_STATE_HOME` to folders of its own and keeps `XDG_CACHE_HOME`, sharing its downloads. The
  state folder's path stays short: a VM's sockets are in it, and macOS caps their paths at 104 bytes.

## Workflow

- `docs/roadmap.md` is the person's backlog: change it only when the person asks. Each item is
  done as a plan and one PR.
- Keep multi-step work as a checklist in `docs/plans/yyyymmdd-<name>.md`, only ticking it during
  the work; when done, add `Done: yyyy-mm-dd` on top and a short `## Result`, and move it to
  `docs/plans/completed/`.
- Before committing, run `make check` and `make test`. `make prepare` rewrites files: not a check.
- Make each commit one meaningful change a person can check: one short lowercase line, no body,
  docs updated in the same commit. Push only when asked.
- When fixing your own unpushed commit, amend or fix up that commit instead of adding a separate
  fix.
- Ask the person only when what trivm does must change, a heavy dependency is needed, or the
  request is ambiguous. Otherwise decide and say so in your report.
- Keep docs to `readme.md` (users), the rustdoc, `agents.md`, the roadmap and the plans. No
  progress journals, run logs or disclaimer paragraphs.
