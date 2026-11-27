# The accessibility tree on Windows

`trivm ui <vm>` and `trivm click <vm> --name <text>` work on Windows 11 as on macOS and Linux: agent-desktop's arm64 build for Windows, which reads UI Automation, put in the guest at the first use, pinned by version and SHA-256 like its other two builds, run in the user's desktop session, its frames in a shot's pixels. Done when, on this Mac, on a fresh `windows-11`, `ui` lists Notepad's and Explorer's windows and elements with frames where the shot has them, and `click --name` clicks a menu and a button by name, raising the window first when another covers it (shots read).

## Decisions

- The tool: agent-desktop v1.3.1's `agent-desktop-v1.3.1-aarch64-pc-windows-msvc.tar.gz` (1,779,550 bytes, Apache-2.0 as its other builds), from its GitHub release, pinned by SHA-256 `805f1f61…`, which the release's `checksums.txt` and GitHub's digest agree on. Its one file, `agent-desktop.exe`, goes to `C:\Program Files\agent-desktop`, the archive piped over SSH into Windows' own `tar`, run by sshd's shell, cmd, which leaves stdin alone. Each run checks its version, as on the others.
- The Visual C++ runtime: the build links `VCRUNTIME140.dll`, which Windows 11 doesn't have (run, it fails at once with `0xC0000135`, a DLL not found). trivm puts in Microsoft's own installer for arm64, 14.52.37029 (`VC_redist.arm64.exe`, from Microsoft's versioned download URL, pinned by SHA-256 `3c81bcf5…`), copied in by `scp` and run with `/install /quiet /norestart`, only when `System32` has no `vcruntime140.dll`. It is Microsoft's runtime for Windows, as the system itself is, installed in the guest, not shipped by trivm. In the cache by its version's name, as the URL's file name doesn't change with it.
- Where it runs: an SSH session has no desktop (agent-desktop there lists no windows), so it runs as `desktop` runs a program: a scheduled task run as the user with the interactive logon type, in the logged-on session, at the highest run level, as `desktop`'s programs run elevated and UI Automation refuses a lower process input into a higher one. The task runs `conhost.exe --headless cmd.exe /c run.cmd`: no console window, which would take the focus and show in shots.
- Its arguments and output: the batch file's one line runs agent-desktop with its output and errors to files, then writes `done`; the PowerShell over SSH waits for it (a minute at most), prints them and removes the task. The line comes on SSH's stdin, not in the command line, which cmd caps at 8191 characters with the script; quoted as Windows splits a command line, then `%` doubled and cmd's operators escaped where cmd reads them (unit tests). A batch's line past 8191 characters is refused, saying `--app` picks fewer windows. A run takes 3–5 seconds, most of it PowerShell and the task's start.
- Frames: agent-desktop makes itself per-monitor DPI aware, so UI Automation gives it the screen's pixels, a shot's from 0,0. Checked at 150%: frames in the shot's pixels, a click at their middle on the element.
- The raise: Windows refuses `SetForegroundWindow` to a program in the background, which agent-desktop is in its task (`focus-window` fails, "did not become foreground"; an Alt press before it didn't help and left Explorer in its keyboard mode). It does let a window restored from minimized take the focus, so `click --name` minimizes and restores the window, as its taskbar button would, then `focus-window` checks it is in front.
- Menus: a XAML app's menu (Notepad's, Explorer's) is a window of its own, `PopupHost`, in front of the app's; its tree's top element is a frameless window UI Automation calls off the screen, so `ui` reads into it instead of leaving it out. The same popup is in the app window's tree too: an element alike in all but its window to one in a window before is left out, unless something in it stays. A window listed before the focused one, as such a popup is, isn't raised before a click: minimized, it would close.

## Tasks

- [ ] 1. agent-desktop's Windows build and the Visual C++ runtime pinned and cached, installed in the guest, run in the desktop's session, its output read.
- [ ] 2. `ui` and `click --name` on Windows: the frames, the popups, the raise, the click through QMP.
- [ ] 3. The proof on a fresh `windows-11` with Notepad and Explorer; `readme.md`, `--help` and `agents.md`'s hacks.

## Checks

- `make check`, `make test`; unit tests for the batch file's line, a popup read once and the windows in front.
- Task 3 on this Mac, with the shots read.
