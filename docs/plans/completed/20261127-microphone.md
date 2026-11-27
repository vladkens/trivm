Done: 2026-11-27

# A microphone in the guest

An app's dictation is tested in a VM only with a microphone that hears something, and the Mac's own must stay out of it: no host microphone and no prompt on the host. `trivm mic <vm> <file>` plays an audio file from the host into a microphone of the guest's, on macOS, Linux and Windows, in real time, so an app recording from the default input hears it. `trivm mic <vm>` puts the microphone in the guest without playing anything, so it is there when the app starts. Done when, on this Mac, in a scratch VM of each system, a recording from the default input made while `mic` plays a test file holds that file's sound (its tones found in the recording on the host), and a shot of the system's sound settings shows the input device (shots read).

## Decisions

- The command: `mic <vm> [file]`, a top-level command beside `shot` and `type`, as the microphone is one more of the VM's inputs and works on all three systems; not under `os`. With a file, it returns once the file has played; sound no app records is lost, as a microphone's is. The microphone is put in the guest at the first use, as `ui` puts agent-desktop, so `mic <vm>` alone is how it is put there before an app starts.
- The file: any format macOS's `afconvert` reads (WAV, AIFF, CAF, MP3, AAC, FLAC), converted on the host to 16-bit mono PCM at 48 kHz, a microphone's format, the same for every system; trivm decodes nothing itself.
- Linux: the audio server's own virtual devices through `pactl`, whose PulseAudio protocol PipeWire's `pipewire-pulse` serves too: a null sink `trivm-mic-in` that the file is played into, and `trivm-mic`, a mono source remapped from its monitor, made the default input. The guest's own sounds go to a second, silent null sink `trivm-speaker`, the default output, so the microphone hears only the file: with `trivm-mic-in` the only sink, PulseAudio's `module-always-sink` would make it the default output. Loaded at once with `pactl` and kept for later logins in `/etc/pulse/default.pa.d` and `/etc/pipewire/pipewire-pulse.conf.d`, each server reading its own. Played by `pacat` from SSH's input, paced by the null sink's clock. Ubuntu 24.04's Xfce has PulseAudio 16.1 and `pactl`, its socket not started in the session that installed it: `mic` starts the server's socket. Debian 13's GNOME has PipeWire 1.4 with `pipewire-pulse` but no `pactl`: `mic` installs `pulseaudio-utils` (with `dnf` on Fedora). PulseAudio's file is written only beside its daemon's `default.pa`: Debian has `/etc/pulse` for the client library alone. The modules are loaded unless the server, started by `pactl` only then, read them from the file just written: on Fedora it had, and the microphone came twice.
- macOS: BlackHole 2ch 0.7.1 (Existential Audio, GPL-3.0), an AudioServerPlugIn, a driver coreaudiod loads with no kernel extension and SIP left as it is: its pkg from existential.audio, pinned by the SHA-256 that Homebrew's `blackhole-2ch` cask has (signed by Existential Audio's Developer ID and notarized), installed with `installer`; coreaudiod restarted, as its scripts don't. A VZ VM has no sound device, so BlackHole is the guest's only device, its default input and output, and `afplay` plays the file into it. No virtio-sound: VZ's only input source is the Mac's microphone (`VZHostAudioInputStreamSource`), which this item rules out, and its output would play the guest's sounds on the Mac. With BlackHole the only output, the guest's own sounds go to the microphone too. An app records only with TCC's microphone grant, which `os grant <vm> <app> microphone` writes. `afplay` into BlackHole needs no grant and puts up no prompt. A recording started over SSH does: macOS asks on the guest's screen whether `sshd-keygen-wrapper` may use the microphone, and coreaudiod holds every client, `afplay` too, until someone answers. So the app that records runs on the desktop, with its own grant.
- Windows: no virtual audio driver for arm64 with a fitting licence: VirtualDrivers' Virtual-Audio-Driver (MIT) is built for x64 only (its signed 25.7.14 has an x86-64 `.sys`), VB-Cable and Virtual Audio Cable are closed, Microsoft's SysVAD sample would have to be built and signed. So QEMU gives Windows a sound card, `ich9-intel-hda` with `hda-micro` (a speaker and a microphone), for Windows' own HD Audio driver, on QEMU's D-Bus audio backend (`-audiodev dbus`, which needs `-display dbus,p2p=yes`; VNC and QMP's screendump stay): nothing reaches the Mac's sound, and the speaker plays to no one. `start` runs the microphone's listener beside QEMU, `trivm _mic <vm>` in a session of its own, until QEMU quits: it hands QEMU a socket for a D-Bus client (QMP's `getfd` and `add_client`), registers an `org.qemu.Display1.AudioInListener` on a second socket (`RegisterInListener`), and QEMU calls its `Read` for the microphone's samples, at the rate the guest records. It answers with silence, or by the clock with the samples of the file that `mic` sends it on `mic.sock`, `mic` returning once they have played. It lives as long as QEMU, as with no listener QEMU's `dbus_read` returns its buffer unwritten: the heap's leftovers, then the last samples again. A second `mic` cuts the first off; a `mic` that quits stops its file. Windows 11's own HD Audio driver takes the card on arm64: a Speakers and a Microphone endpoint (High Definition Audio Device), the defaults. A VM started before this version, with no listener, is told to restart.
- D-Bus: the little of its wire protocol this needs, a small own routine in a file of its own, as `qcow2.rs`: the EXTERNAL authentication, messages with a unix fd, method calls, returns and errors, rather than `zbus` and an async runtime. QEMU's interface is read from its `ui/dbus-display1.xml` and `audio/dbusaudio.c` (11.1): QEMU authenticates the listener's connection as the server and calls `GetAll` on it, `Init`, `SetEnabled`, `Read`.
- Licences: BlackHole runs in the guest as a program of its own, a driver loaded by coreaudiod, downloaded from its author by the person's trivm at the first `mic`, as a distro's packages are; trivm hosts and links nothing of it. No code is taken from QEMU.

## Tasks

- [x] 1. `mic <vm> [file]`: the command, `afconvert` on the host, `--help`, `readme.md`.
- [x] 2. Linux: the virtual devices with `pactl`, kept for later logins, `pacat`.
- [x] 3. macOS: BlackHole pinned, installed and loaded, `afplay`.
- [x] 4. Windows: the sound card on QEMU's D-Bus audio, the D-Bus client, the listener fed from the file.
- [x] 5. The proof on each system: a recording from the default input holding the file's tones, the sound settings' input in a shot.

## Checks

- `make check`, `make test`; unit tests for QEMU's command line, the D-Bus messages, the WAV read.
- On this Mac, in scratch VMs: `mic` on Ubuntu 24.04, macOS 27 and Windows 11, a recording of the default input checked on the host for the test file's tones, the sound settings in shots read; an app started before `mic` and after a restart still has the microphone.

## Result

On this Mac, in scratch VMs beside the person's, with a test file of four tones (440, 880, 1320,
660 Hz, a second each, 44.1 kHz stereo WAV). Each recording of the default input, made while `mic`
played the file, was checked on the host for the tones, in order; shots read:

- Ubuntu 24.04 (Xfce, PulseAudio 16.1): the four tones; after a restart `trivm-mic` was back as the
  default input with no `mic`; pavucontrol's Input Devices show "trivm microphone", its level
  moving.
- Debian 13 (GNOME, PipeWire 1.4): `pactl` installed by the first `mic`; the four tones, the
  recorder stopped as `mic` returned; back after a restart; GNOME's Sound shows "trivm microphone"
  as the input device and "trivm speaker" as the output, the input level moving.
- Fedora 45 (Xfce, PipeWire 1.6): `pactl` installed with `dnf`; the four tones; one set of devices
  after a restart. Ubuntu 22.04 (KDE, PulseAudio 15.99): the four tones; back after a restart.
- macOS 27, a clone of `macos-27`: BlackHole put in by `mic` in 3 seconds, then the default input
  and output (`system_profiler`); a recording by `AVAudioRecorder` in a launchd job, with
  `os grant <vm> /usr/bin/osascript microphone`, held the four tones; Sound's Input shows
  "BlackHole 2ch", Virtual, its input level lit.
- Windows 11 (QEMU 11.1.3): installed with the sound card in 14 minutes, then with its listener
  in 22; a recording by winmm's MCI in the user's session: silent (peak 1) with no `mic`; the
  four tones while `mic` played (0.2 s of one skipped where the guest read late); silent again
  from a `mic` killed 2 seconds in; the four tones after `stop`, which ended the listener with
  QEMU, and `start`. Sound's Input shows "Microphone, High Definition Audio Device, Default
  device", its volume bar moving; `shot`, `click`, `key` as before.
- `mic` with the 5-second file returned in 5.4 to 6.9 seconds; `mic <vm>` alone in 0.3 to 3
  seconds, 10 when `dnf` installed `pactl`.

Found on the way: Debian's GNOME has PipeWire but no `pactl`; on Fedora `pactl` starts a server
that has read the config just written; a recording started over SSH on macOS asks on the guest's
screen for `sshd-keygen-wrapper`'s microphone and stalls `afplay` until it is answered; Windows
11's HD Audio driver works on arm64; QEMU's D-Bus audio takes one listener for each D-Bus display
and, with none, hands the guest its input buffer unwritten (the review found it in its source), so
the listener lives as long as QEMU.

Left: on macOS the guest's own sounds reach the microphone, as BlackHole is its only device; a
Windows VM started before this version gets the sound card and its listener at its next start.
