# trivm

**Test desktop apps on macOS, Linux and Windows VMs on Apple silicon, driven from the terminal by coding agents.**

trivm installs a system into a VM with one command and no clicks, runs it in the background, and
lets an agent (or you) run commands in it, copy files, take screenshots, click and type, all from
the terminal. When you want to look, `trivm open` shows the VM in a window.

- macOS 13, 14, 15, 26 and 27 on Apple's Virtualization.framework.
- Ubuntu 24.04 (Xfce) and 22.04 (KDE Plasma), Debian 13 (GNOME) and Fedora 45 (Xfce), on X11.
- Windows 11 on QEMU with HVF (`brew install qemu swtpm`), from Microsoft's arm64 ISO.
- Systems from their own sources (Apple's IPSWs, the distros' cloud images, Microsoft's ISO),
  pinned by SHA-256.
- No agent in the guest: commands go through SSH, the screen is driven from the host.
- One binary in Rust, no other app driven: no UTM, no AppleScript.

Requires a Mac with Apple silicon on macOS 27.

## Install

```sh
cargo install --git https://github.com/vladkens/trivm
```

trivm signs itself ad hoc with the virtualization entitlement the first time it runs a VM.

## Use

```sh
trivm create macos-27          # downloads 25 GB once, installs, starts: about 20 minutes
trivm create macos-14          # the last IPSW of each macOS: 13.6, 14.6.1, 15.6.1, 26.6.2
trivm create ubuntu-24.04      # downloads 600 MB once: about 3 minutes
trivm list

trivm exec macos-27 -- sw_vers
trivm cp ./MyApp.dmg macos-27:/tmp/
trivm exec macos-27 'hdiutil attach /tmp/MyApp.dmg && cp -R /Volumes/MyApp/MyApp.app /Applications/'
trivm exec macos-27 -- open -a MyApp
trivm exec ubuntu-24.04 'nohup myapp >/dev/null 2>&1 &'   # shows on the desktop

trivm shot macos-27 shot.png   # 1600×900 on macOS, 1920×1080 on Linux, ≤1024×768 on Windows
trivm click macos-27 800 450
trivm type macos-27 'hello'
trivm key macos-27 cmd+q

trivm open macos-27            # a window onto the VM, to look or use the mouse
trivm stop macos-27
```

### macOS 13 and 14

Their Data volume is encrypted once installed, so their Setup Assistant can't be skipped from the
host: `trivm create macos-14` boots into it and waits. Answer it in `trivm open macos-14` (or with
`shot`, `click`, `type` and `key`) with the user `user` and the password `1234`, then turn Remote
Login on in Terminal; trivm goes on once SSH answers.

```sh
sudo launchctl load -w /System/Library/LaunchDaemons/ssh.plist
```

### Windows

Microsoft gives its ISO only to a browser: download Windows 11 (multi-edition ISO for Arm64),
English, from [its page](https://www.microsoft.com/en-us/software-download/windows11arm64) once.

```sh
trivm create windows-11 --iso ~/Downloads/Win11_26H2_English_Arm64.iso   # kept in the cache
trivm exec windows-11 'Get-ComputerInfo OsName, OsVersion'   # PowerShell
trivm cp ./MyApp-setup.exe windows-11:            # to C:\Users\user
trivm exec windows-11 '.\MyApp-setup.exe /S'
trivm exec windows-11 'desktop "C:\Program Files\MyApp\MyApp.exe"'   # shows on the desktop
trivm key windows-11 win
```

`exec` runs PowerShell 5.1 and exits with the last program's code (1 when a cmdlet failed).
PowerShell 5.1 drops a program's empty arguments and the double quotes in them: write such a
command as one line, quoted for PowerShell. An SSH session has no desktop: `desktop <program>
[args]` starts a program in the user's desktop session. `open` hands the VM's screen to Screen
Sharing (VNC on 127.0.0.1, password `1234`). Windows isn't activated.

`create` and `start` return when the desktop is up. Coordinates for `click` are the pixels of a
shot. The guest's user is `user`, password `1234`, sudo without a password (an administrator on
Windows). VMs live in `~/.trivm` (`TRIVM_HOME`), downloads in `~/.trivm/cache`.

## License

MIT
