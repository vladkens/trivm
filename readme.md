# trivm

**Test desktop apps on macOS, Linux and Windows VMs on Apple silicon, driven from the terminal by coding agents.**

trivm installs a system into a VM with one command and no clicks, runs it in the background, and
lets an agent (or you) run commands in it, copy files, take screenshots, click and type, all from
the terminal. When you want to look, `trivm open` shows the VM in a window.

- macOS 27 and Ubuntu 24.04 (Xfce) on Apple's Virtualization.framework; Windows 11 next.
- Systems from their own sources (Apple's IPSW, Ubuntu's cloud image), pinned by SHA-256.
- No agent in the guest: commands go through SSH, the screen is driven from the host.
- One binary in Rust, no other app driven: no UTM, no AppleScript.

Requires a Mac with Apple silicon on macOS 27 (macOS guests are made with its guest provisioning).

## Install

```sh
cargo install --git https://github.com/vladkens/trivm
```

trivm signs itself ad hoc with the virtualization entitlement the first time it runs a VM.

## Use

```sh
trivm create macos-27          # downloads 25 GB once, installs, starts: about 20 minutes
trivm create ubuntu-24.04      # downloads 600 MB once: about 3 minutes
trivm list

trivm exec macos-27 -- sw_vers
trivm cp ./MyApp.dmg macos-27:/tmp/
trivm exec macos-27 'hdiutil attach /tmp/MyApp.dmg && cp -R /Volumes/MyApp/MyApp.app /Applications/'
trivm exec macos-27 -- open -a MyApp
trivm exec ubuntu-24.04 'nohup myapp >/dev/null 2>&1 &'   # shows on the desktop

trivm shot macos-27 shot.png   # 1600×900 on macOS, 1920×1080 on Ubuntu
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
the first use. On macOS it needs SIP off for its grant. An app shows what its toolkit gives
accessibility: on Linux GTK's do, and Qt's and Chromium's once started after the first `ui`.

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

`create` and `start` return when the desktop is up. Coordinates for `click` are the pixels of a
shot. The guest's user is `user`, password `1234`, sudo without a password.

## Files

trivm keeps its files as Linux tools do, by the XDG Base Directory spec, each folder moved by its
variable:

- `~/.config/trivm` (`XDG_CONFIG_HOME`): each VM's settings, `<vm>.conf`.
- `~/.local/share/trivm` (`XDG_DATA_HOME`): each VM's disks and machine, trivm's SSH key.
- `~/.cache/trivm` (`XDG_CACHE_HOME`): downloads and the disks made from them.
- `~/.local/state/trivm` (`XDG_STATE_HOME`): each VM's log, lock and socket.

## License

MIT
