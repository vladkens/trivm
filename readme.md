# trivm

**Test desktop apps on macOS, Linux and Windows VMs on Apple silicon, driven from the terminal by coding agents.**

trivm installs a system into a VM with one command and no clicks, runs it in the background, and
lets an agent (or you) run commands in it, copy files, take screenshots, click and type, all from
the terminal. When you want to look, `trivm open` shows the VM in a window.

- macOS 15, 26 and 27; Ubuntu, Debian and Fedora with their desktops; Windows 11.
- Ubuntu 26.04 with GNOME 50 or KDE Plasma 6.6, on Wayland.
- Systems from their own sources (Apple, the distros, Microsoft), pinned by SHA-256.
- Throwaway clones of a VM, for tests that need a clean system each time.
- The screen's windows and buttons by name, so an agent clicks by name, not by pixel.
- macOS permissions (Accessibility, Screen Recording...) for the app, with no clicks.
- One binary in Rust: no UTM, no AppleScript.

Requires a Mac with Apple silicon on macOS 27; Windows also needs `brew install qemu swtpm wimlib`.

## Install

```sh
cargo install --git https://github.com/vladkens/trivm
```

trivm signs itself ad hoc with the virtualization entitlement the first time it runs a VM.

## Use

```sh
trivm create macos-27        # downloads 25 GB once, installs, starts: about 20 minutes
trivm create ubuntu-24.04    # about 3 minutes
trivm create ubuntu-26.04-kde  # KDE Plasma on Wayland; ubuntu-26.04-gnome for GNOME
trivm create windows-11      # about 25 minutes
trivm list

trivm cp ./MyApp.dmg macos-27:/tmp/
trivm exec macos-27 'hdiutil attach /tmp/MyApp.dmg && cp -R /Volumes/MyApp/MyApp.app /Applications/'
trivm exec macos-27 'desktop /Applications/MyApp.app'   # on the desktop, not in SSH

trivm shot macos-27 shot.png
trivm click macos-27 800 450
trivm type macos-27 'hello'
trivm key macos-27 cmd+q

trivm os grant macos-27 /Applications/MyApp.app   # macOS permissions, no Allow clicked
trivm ui macos-27 --app MyApp                     # its windows and buttons, by name
trivm click macos-27 --name Settings

trivm run macos-27 -- sh test.sh   # on a throwaway clone, its name in TRIVM_VM
trivm open macos-27
trivm stop macos-27
```

`trivm --help` lists the systems and all commands. The guest's user is `user`, password `1234`.

Files, by the XDG Base Directory spec: each VM's settings in `~/.config/trivm/<vm>.conf`, its
disks in `~/.local/share/trivm`, downloads in `~/.cache/trivm`, logs and sockets in
`~/.local/state/trivm`.

## License

MIT
