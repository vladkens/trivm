# Roadmap

The maintainer's backlog, in order: each item is done as a plan in `docs/plans/` and one PR. Agents change it only when the person asks.

- [ ] **macOS and Linux on Virtualization.framework.** `trivm create macos-27` and `ubuntu-24.04` with no clicks; start, stop, list, delete; exec, cp and ssh through SSH; shot, click, type and key through the VM's display from the host; open, a window onto the running VM. Plan: [20261124-vz.md](plans/20261124-vz.md).
- [ ] **Windows 11 on QEMU.** `trivm create windows-11`: Microsoft's arm64 ISO installed unattended (an autounattend with the TPM, Secure Boot and online-account checks skipped, virtio-win's drivers, Win32-OpenSSH), run by Homebrew's `qemu` with HVF and `swtpm`; shot and input over QMP, the window through QEMU's VNC; commands that need the desktop through a task run at logon, as SSH's sessions have none.
- [ ] **Throwaway clones.** `trivm run <vm> -- <cmd>`: an APFS clone of a stopped VM started, the command run, the clone deleted; `trivm clone` keeps one. For tests that need a clean system each time.
- [ ] **Older systems.** macOS 13–26 from their IPSWs with the Setup Assistant skipped as Lima does (`.AppleSetupDone` and a LaunchDaemon that makes the user, written to the Data volume); Ubuntu 22.04 with KDE (Kubuntu's desktop); Debian and Fedora.
- [ ] **The accessibility tree.** `trivm ui <vm>`: the windows and elements of the guest's desktop with their roles, names and frames, through a tool put in the guest (agent-desktop on macOS, Cua Driver on Windows and Linux), so an agent clicks by name, not by pixel.
- [ ] **Permissions for the app under test.** macOS: SIP off from recoveryOS and the TCC grants (Accessibility, Screen Recording, Input Monitoring, the microphone) written for an app by path, as the old `vm` tool did through UTM.

Not planned: x86 guests (emulated), Windows 10, macOS 11 and 12, images hosted by us, an app of our own beyond the VM's window.
