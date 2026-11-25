Done: 2026-11-24

# Permissions for the app under test

An app that needs Accessibility, Screen Recording, Input Monitoring or the microphone shows its warnings in a macOS VM, where no one clicks Allow: trivm turns SIP off from recoveryOS, driving its screen from the host, and writes the grants into the guest's TCC databases for an app given by its path. Done when, on this Mac, `macos-27` has SIP off by trivm, wavo-app v0.3.0's General page shows "Wavo needs two permissions" before a grant and no warning after it and a relaunch (shots read), and a `trivm run macos-27` clone shows the grants carried into it.

## Decisions

- Two commands under `os`, the namespace for the guest system's own integrations, not top-level commands, as they are one system's and low-level: `os grant <vm> <app> [<service>...]`, fast, for each app, and `os sip <vm> on|off`. `os grant` turns SIP off itself when it's on, by the same recoveryOS steps (a minute, once per VM), with one line saying so, so a person normally calls only `grant`; it finds the app first, so a wrong path costs no minute. Both start a VM that is stopped and leave it up. `os sip` is kept for putting a VM back: `os sip on` is the same steps with `csrutil enable`; `csrutil status` over `exec` reads the state. On a system without them (Linux) each says "os <command>: not supported on <system>"; both refuse a VM not set up. A guest system's integrations to come go under `os` too.
- recoveryOS: `trivm _run <vm> --recovery` starts the VM with `VZMacOSVirtualMachineStartOptions.startUpFromMacOSRecovery`. `os sip` starts the VM if it is stopped and reads `csrutil status` (nothing to do: done), stops it, runs it so, drives recoveryOS through the display as `click`, `type` and `key` do, has `csrutil disable` typed in its Terminal, halts it, starts the VM as `start` does and checks `csrutil status` again; `os grant` with SIP on does the same. The VM is up after.
- recoveryOS of 27.2, found with shots: the startup options within 10 s (the disk and Options; Options chosen shows Continue), recoveryOS's assistant 5 s later with no user to pick, ⇧⌘T opens Terminal (a root shell), `csrutil disable` asks `[y/n]`, then "Enter password for user user" (the only admin), then says "System Integrity Protection is off."; `halt` ends the VM in 5 s. `csrutil enable` asks the same.
- Each step waits for its screen's text, read by Vision's `VNRecognizeTextRequest` from a shot (Vision loaded with `dlopen` at the first use, called through `msg_send!` as the provisioning options are): no fixed waits, which the old tool needed (75 s each), and a step that never comes fails with the shot's path. Both recognition levels: the accurate one read nothing at all in one Terminal shot where the fast one read its title. The first recognition on a Mac took 24 s (Vision's model loaded), later ones 0.1 s.
- The grants go where macOS keeps them: Accessibility (with PostEvent, sending events), Screen Recording (ScreenCapture) and Input Monitoring (ListenEvent) in the system's `/Library/Application Support/com.apple.TCC/TCC.db`, the microphone in the user's. On macOS 27 the user's is not in `~/Library` (a database made there is ignored) but in tccd's own container, `/private/var/containers/Data/ProtectedSystem/<uuid>/Data/Library/Application Support/com.apple.TCC/TCC.db`, the one whose `.com.apple.containermanagerd.metadata.plist` names `com.apple.tccd` and the user's uid; `os grant` finds it so, and falls back to `~/Library` for older systems. Written as root with `sqlite3` (SIP off lets root write them). All four services by default.
- macOS 27's `access` table: service, client, client_type, auth_value, auth_reason, auth_version, csreq, policy_id, indirect_object_identifier_type, indirect_object_identifier, indirect_object_code_identity, flags, last_modified, pid, pid_version, boot_uuid, last_reminded, one_time_reprompt_eligible, reminder_count. A row is (service, client, client_type, 2 allowed, 4 by the system's policy, 1, NULL csreq, 0 flags); the rest take their defaults.
- The client, tried on Wavo: a bundle by its bundle identifier (client_type 0) with no csreq is honoured, and tccd leaves the csreq empty; by the path of its executable (client_type 1) it is not (tccd adds a denied row of the bundle identifier beside it). So `os grant` reads `CFBundleIdentifier` for a bundle and gives a bare executable its path with its links resolved (client_type 1), as tccd sees the process, which tccd does honour: `/usr/bin/osascript` run by launchd as its own responsible process read false for AXIsProcessTrusted and the screen, listen and post preflights before, true for all four after. No csreq: it would pin an ad hoc signed app's hash, so a rebuild would lose the grant. A link's own path is not honoured: `/usr/local/bin/x-osascript`, a link to `/usr/bin/osascript`, granted so read false, and tccd added a denied row of `/usr/bin/osascript`.
- tccd reads its databases once and ignores SIGTERM: `pkill -9 -x tccd` after writing, and launchd starts it on the next request; none running is fine, as after a grant a moment before.
- The microphone: Wavo reads `AVCaptureDevice`'s status for audio, which is TCC's answer alone; a VM has no audio input device, and none is needed for the grant to show. Recording itself would need one: not added.
- Clones: SIP's state and the TCC databases are on the VM's disk and auxiliary storage, which `clone` copies, so a grant made on a stopped base VM is in each of its clones.

## Tasks

- [x] 1. `_run --recovery`: the VM started into recoveryOS; its screens, the keys and clicks it takes, the times, found with shots.
- [x] 2. `os sip <vm> on|off`: recoveryOS driven from the host, `csrutil disable`, the VM back up, `csrutil status` checked.
- [x] 3. `os grant <vm> <app> [<service>...]`: the TCC tables of macOS 27, by-path or bundle grants, the csreq, `tccd` restarted, SIP turned off first when it's on.
- [x] 4. The proof: SIP off on `macos-27`; Wavo's warnings before and gone after `os grant`; the same through a `run` clone; `readme.md` and `--help`.

## Checks

- `make check`, `make test`; unit tests for the arguments and the SQL written.
- `trivm os sip macos-27 off` on this Mac, its screens read in shots, then `csrutil status` says disabled.
- `trivm os grant macos-27 /Applications/Wavo.app`: Wavo's General page before and after, in shots read; the same in a `run` clone of `macos-27`; on a clone with SIP on, `os grant` turns it off first; refused on `ubuntu-24.04`.

## Result

On this Mac (macOS 27.2, Apple silicon), guest macOS 27.2:

- `sip macos-27 off`: 1:12, recoveryOS driven from its startup options to `csrutil disable` and
  back, then `csrutil status` over SSH said disabled. A clone of it: `sip on` 1:06, `sip off` 50 s,
  `sip on` again with nothing to do 0.3 s. One `sip on` turned SIP on but failed its last check: the
  guest, up 12 s at load 45, dropped the SSH session (exit 255, no message); the check now asks up
  to three times.
- `grant macos-27 /Applications/Wavo.app`: 0.4 s. wavo-app v0.3.0 (its release's DMG, SHA-256
  checked) showed "Wavo needs two permissions", Microphone and Accessibility with Allow, before it;
  after it and a relaunch, the General page with no warning (shots `permissions-before.png`,
  `permissions-after.png`). The microphone's status needs no audio device.
- `grant macos-27 /usr/bin/osascript` (a bare executable, by its path): run by launchd, it read
  false for Accessibility, Screen Recording, Input Monitoring and posting events before and true
  for all four after. Its rows were removed after.
- `run macos-27 -- sh permissions-run.sh`: 36 s; the clone had SIP off and Wavo's rows, Wavo opened
  at its login with no warning (shot `permissions-run.png`), and the clone was deleted after.
- Refused, each with its own message: `grant` and `sip` on `ubuntu-24.04`, `grant` on the clone
  with SIP on, an unknown service, a relative path, an app that isn't there, an unknown VM.

`macos-27` is left stopped with SIP off and Wavo installed and granted: the next item, the
accessibility tree, needs both. Left: recording from the microphone would need an audio input
device in the VM, which trivm doesn't add; recoveryOS's screens were read on macOS 27.2 only, and
an older macOS's recoveryOS may need other steps.

From the review of the whole stack (it timed out at 540 s twice, then ran), in `vz` and `clones`'
code, each checked here: a VM's folder is made for its owner only (0700; the home folder's group
`staff` has every user of the Mac), a clone's checked so, and a clone's staging folder left from
before is refused again, not reused; `stop` no longer waits for the shutdown command past its 2
minutes nor for the answer to its `kill` request past 5 s (a busy VM process answered it after
up to 300 s), and takes the VM as gone once its own process is, so it never follows the VM
started again meanwhile (a 2 GB Ubuntu clone stopped from inside in 3 s, with `--force` in 0.3 s);
the names of trivm's own files (`id_ed25519.pub`, `entitlements.plist`, ...) are refused as VM
names, in any case (APFS ignores it: `CACHE` was the cache). Folders made before keep their mode. Not changed: the reaper's stop of a clone another
process replaced between its inode check and its kill request, a few milliseconds where a
replacement takes seconds to make and start.

Merged onto #2's final code, the XDG folders: the folders' mode, the reserved names and `stop`'s 2
minutes are #1's versions now (each folder made 0700; the reserved names are the SSH key's files
beside the VMs in data, where `cache` and `entitlements.plist` no longer are), and a clone has no
staging folder, its name claimed by its data folder. `stop`'s kill awaited 5 s at most and its VM
gone once its own process is stay, as do `sip` and `grant`.

Moved under `os` after #4's review, as top-level commands they were one system's low-level ones
among the VM's own: `os grant` and `os sip`, and `os grant` turns SIP off itself, where `grant`
refused a VM with SIP on. On `g1`, a clone of `macos-27`: `os sip g1 on` 1:23, then `csrutil
status` said enabled and Wavo showed "Wavo needs two permissions" (shot `pr4-os-before.png`); `os
grant g1 /Applications/Wavo.app` said "SIP is on in g1: turning it off from recoveryOS first (about
a minute)" and granted in 58 s, `csrutil status` said disabled, and Wavo relaunched showed no
warning (shot `pr4-os-after.png`). With SIP off, `os grant` took no recoveryOS step and `os sip g1
off` only said SIP is off. `os grant` and `os sip` on `ubuntu-24.04` said "os grant: not supported
on ubuntu-24.04" (and `os sip`), exit 1, the VM left stopped; `sip` and `grant` are unknown
commands now. `g1` was deleted after, its files gone; `macos-27` was not touched.

From #4's review, each reproduced on `g2`, a clone of `macos-27`, then fixed: a grant right after
another failed, its writes done, as `killall` found no tccd ("No matching processes were found",
the third of three); with `pkill`, whose exit 1 means none, four in a row passed with no tccd
running. A link to an executable was granted by the link's path, which tccd ignores:
`AXIsProcessTrusted` read false for `/usr/local/bin/x-osascript` (a link to `/usr/bin/osascript`)
run by launchd after it; granted by the resolved path now, it read true. `g2` was deleted after.
