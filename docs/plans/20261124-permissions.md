# Permissions for the app under test

An app that needs Accessibility, Screen Recording, Input Monitoring or the microphone shows its warnings in a macOS VM, where no one clicks Allow: trivm turns SIP off from recoveryOS, driving its screen from the host, and writes the grants into the guest's TCC databases for an app given by its path. Done when, on this Mac, `macos-27` has SIP off by trivm, wavo-app v0.3.0's General page shows "Wavo needs two permissions" before a grant and no warning after it and a relaunch (shots read), and a `trivm run macos-27` clone shows the grants carried into it.

## Decisions

- Two commands: `sip <vm> on|off`, once per VM (recoveryOS takes a minute), and `grant <vm> <app> [<service>...]`, fast, for each app; `grant` refuses a VM with SIP on, naming `sip`. Both refuse a Linux VM and one not set up. `sip on` is the same steps with `csrutil enable`, so a VM can be put back; `csrutil status` over `exec` reads the state.
- recoveryOS: `trivm _run <vm> --recovery` starts the VM with `VZMacOSVirtualMachineStartOptions.startUpFromMacOSRecovery`. `sip` starts the VM if it is stopped and reads `csrutil status` (nothing to do: done), stops it, runs it so, drives recoveryOS through the display as `click`, `type` and `key` do, has `csrutil disable` typed in its Terminal, halts it, starts the VM as `start` does and checks `csrutil status` again. The VM is up after.
- recoveryOS of 27.2, found with shots: the startup options within 10 s (the disk and Options; Options chosen shows Continue), recoveryOS's assistant 5 s later with no user to pick, ⇧⌘T opens Terminal (a root shell), `csrutil disable` asks `[y/n]`, then "Enter password for user user" (the only admin), then says "System Integrity Protection is off."; `halt` ends the VM in 5 s. `csrutil enable` asks the same.
- Each step waits for its screen's text, read by Vision's `VNRecognizeTextRequest` from a shot (Vision loaded with `dlopen` at the first use, called through `msg_send!` as the provisioning options are): no fixed waits, which the old tool needed (75 s each), and a step that never comes fails with the shot's path. Both recognition levels: the accurate one read nothing at all in one Terminal shot where the fast one read its title. The first recognition on a Mac took 24 s (Vision's model loaded), later ones 0.1 s.
- The grants go where macOS keeps them: Accessibility (with PostEvent, sending events), Screen Recording (ScreenCapture) and Input Monitoring (ListenEvent) in the system's `/Library/Application Support/com.apple.TCC/TCC.db`, the microphone in the user's. On macOS 27 the user's is not in `~/Library` (a database made there is ignored) but in tccd's own container, `/private/var/containers/Data/ProtectedSystem/<uuid>/Data/Library/Application Support/com.apple.TCC/TCC.db`, the one whose `.com.apple.containermanagerd.metadata.plist` names `com.apple.tccd` and the user's uid; `grant` finds it so, and falls back to `~/Library` for older systems. Written as root with `sqlite3` (SIP off lets root write them). All four services by default.
- macOS 27's `access` table: service, client, client_type, auth_value, auth_reason, auth_version, csreq, policy_id, indirect_object_identifier_type, indirect_object_identifier, indirect_object_code_identity, flags, last_modified, pid, pid_version, boot_uuid, last_reminded, one_time_reprompt_eligible, reminder_count. A row is (service, client, client_type, 2 allowed, 4 by the system's policy, 1, NULL csreq, 0 flags); the rest take their defaults.
- The client, tried on Wavo: a bundle by its bundle identifier (client_type 0) with no csreq is honoured, and tccd leaves the csreq empty; by the path of its executable (client_type 1) it is not (tccd adds a denied row of the bundle identifier beside it). So `grant` reads `CFBundleIdentifier` for a bundle and gives a bare executable its path (client_type 1), which tccd does honour: `/usr/bin/osascript` run by launchd as its own responsible process read false for AXIsProcessTrusted and the screen, listen and post preflights before, true for all four after. No csreq: it would pin an ad hoc signed app's hash, so a rebuild would lose the grant.
- tccd reads its databases once and ignores SIGTERM: `killall -9 tccd` after writing, and launchd starts it on the next request.
- The microphone: Wavo reads `AVCaptureDevice`'s status for audio, which is TCC's answer alone; a VM has no audio input device, and none is needed for the grant to show. Recording itself would need one: not added.
- Clones: SIP's state and the TCC databases are on the VM's disk and auxiliary storage, which `clone` copies, so a grant made on a stopped base VM is in each of its clones.

## Tasks

- [x] 1. `_run --recovery`: the VM started into recoveryOS; its screens, the keys and clicks it takes, the times, found with shots.
- [x] 2. `sip <vm> on|off`: recoveryOS driven from the host, `csrutil disable`, the VM back up, `csrutil status` checked.
- [x] 3. `grant <vm> <app> [<service>...]`: the TCC tables of macOS 27, by-path or bundle grants, the csreq, `tccd` restarted.
- [ ] 4. The proof: SIP off on `macos-27`; Wavo's warnings before and gone after `grant`; the same through a `run` clone; `readme.md` and `--help`.

## Checks

- `make check`, `make test`; unit tests for the arguments and the SQL written.
- `trivm sip macos-27 off` on this Mac, its screens read in shots, then `csrutil status` says disabled.
- `trivm grant macos-27 /Applications/Wavo.app`: Wavo's General page before and after, in shots read; the same in a `run` clone of `macos-27`; `grant` refused on `ubuntu-24.04` and on a VM with SIP on.
