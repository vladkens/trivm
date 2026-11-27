# trivm

**Test desktop apps on macOS, Linux and Windows VMs on Apple silicon, driven from the terminal by coding agents.**

trivm installs a system into a VM with one command and no clicks, runs it in the background, and
lets an agent (or you) run commands in it, copy files, take screenshots, click and type, all from
the terminal. When you want to look, `trivm open` shows the VM in a window.

- macOS 15, 26 and 27 on Apple's Virtualization.framework.
- Ubuntu 24.04 (Xfce) and 22.04 (KDE Plasma), Debian 13 (GNOME) and Fedora 45 (Xfce), on X11.
- Windows 11 Pro on QEMU with HVF, from the ESD of Microsoft's Media Creation Tool.
- Systems from their own sources (Apple's IPSWs, the distros' cloud images, Microsoft's ESD),
  pinned by SHA-256.
- No agent in the guest: commands go through SSH, the screen is driven from the host.
- One binary in Rust, no other app driven: no UTM, no AppleScript.

Requires a Mac with Apple silicon on macOS 27; Windows also needs `brew install qemu swtpm wimlib`.

## Install

```sh
cargo install --git https://github.com/vladkens/trivm
```

trivm signs itself ad hoc with the virtualization entitlement the first time it runs a VM.

## Use

```sh
trivm create macos-27          # downloads 25 GB once, installs, starts: about 20 minutes
trivm create macos-15          # the last IPSW of each older macOS: 15.6.1, 26.6.2
trivm create ubuntu-24.04      # downloads 600 MB once: about 3 minutes
trivm list

trivm exec macos-27 -- sw_vers
trivm cp ./MyApp.dmg macos-27:/tmp/
trivm exec macos-27 'hdiutil attach /tmp/MyApp.dmg && cp -R /Volumes/MyApp/MyApp.app /Applications/'
trivm exec macos-27 -- open -a MyApp
trivm exec ubuntu-24.04 'nohup myapp >/dev/null 2>&1 &'   # shows on the desktop

trivm shot macos-27 shot.png   # 1600×900 on macOS, 1920×1080 on Linux, 800×600 on Windows
trivm click macos-27 800 450
trivm type macos-27 'hello'
trivm key macos-27 cmd+q

trivm open macos-27            # a window onto the VM, to look or use the mouse
trivm stop macos-27
```

A test that needs a clean system each time runs against a throwaway clone of a stopped VM: an
APFS copy made in a second that shares the VM's disk until written. `run` starts one, runs the
command on the host with the clone's name in `TRIVM_VM`, deletes the clone however the command
ends (Ctrl+C too), and exits with the command's code:

```sh
cat > test.sh <<'EOF'
set -e
trivm cp ./MyApp.dmg "$TRIVM_VM":/tmp/
trivm exec "$TRIVM_VM" 'hdiutil attach /tmp/MyApp.dmg && cp -R /Volumes/MyApp/MyApp.app /Applications/'
trivm exec "$TRIVM_VM" -- open -a MyApp
trivm shot "$TRIVM_VM" myapp.png
EOF
trivm run macos-27 -- sh test.sh
trivm run ubuntu-24.04 'trivm exec "$TRIVM_VM" -- uname -a'

trivm clone macos-27 macos-27-clean   # a clone to keep, with its own address
```

An app that needs macOS's permissions gets them with no one clicking Allow: `os grant` writes the
grants for the app, by its path in the guest, into macOS's TCC databases, first turning System
Integrity Protection off from recoveryOS, which trivm drives itself (about a minute, once per VM).
Accessibility, Screen Recording, Input Monitoring and the microphone, or the ones named; an app
that runs sees them once relaunched. A VM's clones have its grants. `os` holds the guest system's
own integrations; on a system without one, it says it's not supported there:

```sh
trivm os grant macos-27 /Applications/Wavo.app
trivm os grant macos-27 /usr/local/bin/tool screen input
trivm os sip macos-27 on                # SIP back on, if a test needs it
```

An agent can read the screen as named elements and click them by name: `ui` lists the windows on
the guest's desktop (and macOS's menu bar) with their elements, a line each with the role, name,
value, state and frame in a shot's pixels, read through the system's accessibility by
[agent-desktop](https://github.com/vercel-labs/agent-desktop), which trivm puts in the guest at
the first use. On macOS its grant needs SIP off (`trivm os sip <vm> off`); on Windows it needs
Microsoft's Visual C++ runtime, which trivm puts in too, and `click --name` raises a covered
window by minimizing and restoring it, as Windows lets no program in the background bring one to
the front; a covered menu or tip is uncovered by minimizing the windows in front of it.
`--app` takes an app's name in any case, `notepad` for Windows' `Notepad.exe`.
An app shows what its toolkit gives accessibility: on Linux GTK's do, and Qt's and Chromium's once
started after the first `ui`. Not supported yet on Ubuntu 22.04 (agent-desktop needs 24.04's
accessibility).

```sh
trivm ui macos-27 --app Wavo
# Wavo: menubar 0,0 1600x30
#   menuitem "Wavo" 43,0 56x30
# Wavo: window "Wavo" [focused] 520,78 560x593
#   tablist 520,110 560x48
#     radiobutton "General" [checked selected] 654,110 56x48
#     radiobutton "Models" 710,110 56x48
#   combobox "Microphone" = "Automatic" 939,211 111x24
trivm click macos-27 --name Models   # raises Wavo's window first if another is in front
```

### Windows

```sh
trivm create windows-11        # downloads 4.5 GB once, makes the media, installs: about 25 minutes
trivm exec windows-11 'Get-ComputerInfo OsName, OsVersion'   # PowerShell
trivm cp ./MyApp-setup.exe windows-11:            # to C:\Users\user
trivm exec windows-11 'Start-Process -Wait .\MyApp-setup.exe /S'
trivm exec windows-11 'desktop "C:\Program Files\MyApp\MyApp.exe"'   # shows on the desktop
trivm key windows-11 win
trivm run windows-11 'trivm exec "$TRIVM_VM" -- cmd /c ver'   # a throwaway clone too
```

`exec` runs PowerShell 5.1 and exits with the last program's code (1 when a cmdlet failed); it
doesn't wait for a windowed program, a setup among them, which `Start-Process -Wait` does.
PowerShell 5.1 drops a program's empty arguments and the double quotes in them: write such a
command as one line, quoted for PowerShell. An SSH session has no desktop: `desktop <program>
[args]` starts a program in the user's desktop session. `open` hands the VM's screen to Screen
Sharing (VNC on 127.0.0.1, password `1234`). Windows isn't activated.

trivm makes Windows' install media as Microsoft's Media Creation Tool does: the English arm64 ESD
its catalog lists, from Microsoft's CDN, turned into an ISO of Windows 11 Pro with `wimlib-imagex`
and `hdiutil` and kept in the cache (5 GB).

`create` and `start` return when the desktop is up. Coordinates for `click` are the pixels of a
shot. The guest's user is `user`, password `1234`, sudo without a password (an administrator on
Windows).

## Files

trivm keeps its files as Linux tools do, by the XDG Base Directory spec, each folder moved by its
variable:

- `~/.config/trivm` (`XDG_CONFIG_HOME`): each VM's settings, `<vm>.conf`.
- `~/.local/share/trivm` (`XDG_DATA_HOME`): each VM's disks and machine, trivm's SSH key.
- `~/.cache/trivm` (`XDG_CACHE_HOME`): downloads and the disks made from them.
- `~/.local/state/trivm` (`XDG_STATE_HOME`): each VM's log, lock and sockets.

## License

MIT
