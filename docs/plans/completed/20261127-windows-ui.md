Done: 2026-11-27

# The accessibility tree on Windows

`trivm ui <vm>` and `trivm click <vm> --name <text>` work on Windows 11 as on macOS and Linux: agent-desktop's arm64 build for Windows, which reads UI Automation, put in the guest at the first use, pinned by version and SHA-256 like its other two builds, run in the user's desktop session, its frames in a shot's pixels. Done when, on this Mac, on a fresh `windows-11`, `ui` lists Notepad's and Explorer's windows and elements with frames where the shot has them, and `click --name` clicks a menu and a button by name, raising the window first when another covers it (shots read).

## Decisions

- The tool: agent-desktop v1.3.1's `agent-desktop-v1.3.1-aarch64-pc-windows-msvc.tar.gz` (1,779,550 bytes, Apache-2.0 as its other builds), from its GitHub release, pinned by SHA-256 `805f1f61…`, which the release's `checksums.txt` and GitHub's digest agree on. Its one file, `agent-desktop.exe`, goes to `C:\Program Files\agent-desktop`, the archive piped over SSH into Windows' own `tar`, run by sshd's shell, cmd, which leaves stdin alone. Each run checks its version, as on the others.
- The Visual C++ runtime: the build links `VCRUNTIME140.dll`, which Windows 11 doesn't have (run, it fails at once with `0xC0000135`, a DLL not found). trivm puts in Microsoft's own installer for arm64, 14.52.37029 (`VC_redist.arm64.exe`, from Microsoft's versioned download URL, pinned by SHA-256 `3c81bcf5…`), copied in by `scp` and run with `/install /quiet /norestart`, only when `System32` has no `vcruntime140.dll`. It is Microsoft's runtime for Windows, as the system itself is, installed in the guest, not shipped by trivm. In the cache by its version's name, as the URL's file name doesn't change with it.
- Where it runs: an SSH session has no desktop (agent-desktop there lists no windows), so it runs as `desktop` runs a program: a scheduled task run as the user with the interactive logon type, in the logged-on session, at the highest run level, as `desktop`'s programs run elevated and UI Automation refuses a lower process input into a higher one. The task runs `conhost.exe --headless cmd.exe /c run.cmd`: no console window, which would take the focus and show in shots.
- Its arguments and output: the batch file's one line runs agent-desktop with its output and errors to files, then writes `done`; the PowerShell over SSH waits for it (a minute at most), prints them and removes the task. The line comes on SSH's stdin, not in the command line, which cmd caps at 8191 characters with the script; quoted as Windows splits a command line, then `%` doubled and cmd's operators escaped where cmd reads them (unit tests). A batch's line past 8191 characters is refused, saying `--app` picks fewer windows. A run takes 3–5 seconds, most of it PowerShell and the task's start.
- `--app`: Windows names an app by its program, `Notepad.exe`, so there `.exe` is optional, as the case is everywhere.
- Frames: agent-desktop makes itself per-monitor DPI aware, so UI Automation gives it the screen's pixels, a shot's from 0,0. Checked at 150%: frames in the shot's pixels, a click at their middle on the element.
- The raise: Windows refuses `SetForegroundWindow` to a program in the background, which agent-desktop is in its task (`focus-window` fails, "did not become foreground"; an Alt press before it didn't help and left Explorer in its keyboard mode). It does let a window restored from minimized take the focus, so `click --name` minimizes and restores the window, as its taskbar button would, then `focus-window` checks it is in front. The first window at the click's point, from the front, must then be the element's: one always on top, or a popup, may cover it (an error says which). Of the windows agent-desktop lists: it leaves out tool windows (`WS_EX_TOOLWINDOW`), so one of those always on top goes unseen, as every window does on macOS, which has no such check.
- Menus: a XAML app's menu (Notepad's, Explorer's) is a window of its own, `PopupHost`, in front of the app's; its tree's top element is a frameless window UI Automation calls off the screen, so `ui` reads into it instead of leaving it out. The same popup is in the app window's tree too: a popup's element alike in all but its window to one in a window before is left out, unless something in it stays; other windows' twins all stay. A window listed before the focused one, as such a popup is, isn't raised before a click: minimized, it would close. All in a `PopupHost` window or under a `PopupHost` element is a popup's: a tip's top has a frame, which the frameless rule alone missed. A popup's window behind another is raised by its process's one other window: the popup's own doesn't take the focus. Of several, as Notepad keeps all its windows in one process, `click --name` says so and raises none: agent-desktop doesn't give a window's owner.

## Tasks

- [x] 1. agent-desktop's Windows build and the Visual C++ runtime pinned and cached, installed in the guest, run in the desktop's session, its output read.
- [x] 2. `ui` and `click --name` on Windows: the frames, the popups, the raise, the click through QMP.
- [x] 3. The proof on a fresh `windows-11` with Notepad and Explorer; `readme.md`, `--help` and `agents.md`'s hacks.

## Checks

- `make check`, `make test`; unit tests for the batch file's line, a popup read once and the windows in front.
- Task 3 on this Mac, with the shots read.

## Result

On this Mac, in folders of its own, a fresh `trivm create windows-11` from this branch (main's
ramfb, 800×600: the screen at 1920×1080 is the other branch's), Notepad and Explorer opened by
`desktop`, shots read:

- The first `ui`: downloaded and checked agent-desktop's Windows build and Microsoft's Visual C++
  runtime, put both in, then listed Explorer's window, maximized, and Notepad's with their
  elements, Notepad's tip in its `PopupHost` (26 s; later runs 5–7 s). Their frames are where
  `windows-ui-3.png` shows them: the toolbar's buttons, the tree's items, the status bar.
- `click --name Edit`: Notepad, behind Explorer, raised and its Edit menu open
  (`windows-ui-4-edit.png`, 20 s); `click --name Time/Date`, an item of that menu, wrote the time
  into Notepad (`windows-ui-5-timedate.png`).
- `click --name "More options"`: Explorer, behind Notepad now, raised and its menu open
  (`windows-ui-6-more.png`); `ui --app explorer.exe` listed the menu's items where the shot has
  them; `click --name Options` opened Folder Options (`windows-ui-7-options.png`).
- `click --name View`: three elements of that name, listed with their frames, nothing clicked;
  `click 102 115`, the tab's, showed its View tab (`windows-ui-9-viewtab.png`); `click --name
  Cancel`, a Win32 dialog's button, closed it (`windows-ui-10-cancel.png`).
- Before: at 150% on another VM, frames in the shot's pixels and clicks on them
  (`windows-ui-x11-150.png`, `windows-ui-x12-150-edit.png`).

The fresh VM showed Start open at its first sign-in; `ui` doesn't list it, as agent-desktop leaves
out the shell's windows (the taskbar too), and `key escape` closed it. Left: the roadmap's item is
in the other branch's roadmap, to be ticked once both are in.

A check on a fresh `windows-11` after the PR was opened found three defects, fixed after:

- The first `ui` failed: `Remove-Item` of the runtime's installer in `%TEMP%`, held a moment after
  it exits (by it or Defender). Now tried for 10 s, then left. In the guest, against a file another
  process held: let go after 5 s, removed at 6 s; held 30 s, left at 10 s, no error.
- `--app Notepad` found nothing: `.exe` optional on Windows (unit tests).
- Notepad's tip was listed twice, and `click --name "Got it"` raised its popup's window, which
  failed and hid it: read once, raised by Notepad's window (unit tests for both).

On another fresh `windows-11`: the first `ui --app Notepad` put in agent-desktop and the runtime
and listed Notepad, the installer gone from `%TEMP%`; `--app Explorer` and `--app explorer.EXE`
listed Explorer; `click --name "Got it" --app notepad` raised Notepad from behind Explorer and
closed its tip (`windows-ui-14-gotit.png`).
