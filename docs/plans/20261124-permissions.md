# Permissions for the app under test

An app that needs Accessibility, Screen Recording, Input Monitoring or the microphone shows its warnings in a macOS VM, where no one clicks Allow: trivm turns SIP off from recoveryOS, driving its screen from the host, and writes the grants into the guest's TCC databases for an app given by its path. Done when, on this Mac, `macos-27` has SIP off by trivm, wavo-app v0.3.0's General page shows "Wavo needs two permissions" before a grant and no warning after it and a relaunch (shots read), and a `trivm run macos-27` clone shows the grants carried into it.

## Decisions

- Two commands: `sip <vm> off`, once per VM (recoveryOS takes minutes), and `grant <vm> <app> [<service>...]`, fast, for each app; `grant` refuses a VM with SIP on, naming `sip`. Both refuse a Linux VM. `sip` has `off` only: what the tests need; `csrutil status` over `exec` reads it.
- recoveryOS: `trivm _run <vm> --recovery` starts the VM with `VZMacOSVirtualMachineStartOptions.startUpFromMacOSRecovery`; `sip` stops the VM, runs it so, drives recoveryOS through the display as `shot`, `click`, `type` and `key` do, has `csrutil disable` typed in its Terminal, halts it, starts the VM as `start` does and checks `csrutil status` over SSH. The VM is up after.
- The grants go where macOS keeps them: Accessibility (with PostEvent, sending events), Screen Recording (ScreenCapture) and Input Monitoring (ListenEvent) in the system's `/Library/Application Support/com.apple.TCC/TCC.db`, the microphone in the user's `~/Library/Application Support/com.apple.TCC/TCC.db`; written as root with `sqlite3` (SIP off lets root write them), then `tccd` restarted. All four services by default.
- Clones: SIP's state and the TCC databases are on the VM's disk and auxiliary storage, which `clone` copies, so a grant made on a stopped base VM is in each of its clones.

## Tasks

- [ ] 1. `_run --recovery`: the VM started into recoveryOS; its screens, the keys and clicks it takes, the times, found with shots.
- [ ] 2. `sip <vm> off`: recoveryOS driven from the host, `csrutil disable`, the VM back up, `csrutil status` checked.
- [ ] 3. `grant <vm> <app> [<service>...]`: the TCC tables of macOS 27, by-path or bundle grants, the csreq, `tccd` restarted.
- [ ] 4. The proof: SIP off on `macos-27`; Wavo's warnings before and gone after `grant`; the same through a `run` clone; `readme.md` and `--help`.

## Checks

- `make check`, `make test`; unit tests for the arguments and the SQL written.
- `trivm sip macos-27 off` on this Mac, its screens read in shots, then `csrutil status` says disabled.
- `trivm grant macos-27 /Applications/Wavo.app`: Wavo's General page before and after, in shots read; the same in a `run` clone of `macos-27`; `grant` refused on `ubuntu-24.04` and on a VM with SIP on.
