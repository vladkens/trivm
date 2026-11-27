// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! Windows' guest side: the unattended install's `autounattend.xml` and the setup script its
//! first logon runs, and commands run as PowerShell through SSH, `desktop` among them.

use std::io;

use crate::bad;
use crate::vm::{PASSWORD, USER};

/// Microsoft's generic key for installing Windows 11 Pro; it doesn't activate it.
const PRO_KEY: &str = "VK7JG-NPHTM-C97JM-9MPGT-3V66T";

/// A string as PowerShell reads it literally.
pub fn quote(x: &str) -> String {
  format!("'{}'", x.replace('\'', "''"))
}

/// The command line in PowerShell: one argument is a line as it is, more are a command and its
/// arguments, quoted each.
pub fn line(cmd: &[String]) -> String {
  match cmd {
    [one] => one.clone(),
    _ => format!("& {}", cmd.iter().map(|x| quote(x)).collect::<Vec<_>>().join(" ")),
  }
}

/// Defined before every command: output as UTF-8 without progress records (CLIXML over SSH),
/// and `desktop`, which starts a program in the desktop's session through a scheduled task run as
/// the user with the interactive logon type, as an SSH session has no desktop.
const PRELUDE: &str = r#"$ProgressPreference = 'SilentlyContinue'
try { [Console]::OutputEncoding = [Text.Encoding]::UTF8 } catch {}
$OutputEncoding = [Text.Encoding]::UTF8
function desktop {
  if (-not $args) { throw 'usage: desktop <program> [args]' }
  $exe = "$($args[0])"
  $app = Get-Command $exe -CommandType Application -ErrorAction SilentlyContinue
  if ($app) { $exe = @($app)[0].Source }
  # Quoted as Windows splits a command line: backslashes before a quote, or the closing one, doubled.
  $rest = @($args | Select-Object -Skip 1 | ForEach-Object {
    if ("$_" -notmatch '[\s"]' -and "$_" -ne '') { "$_"; return }
    '"' + ("$_" -replace '(\\*)"', '$1$1\"' -replace '(\\+)$', '$1$1') + '"'
  }) -join ' '
  $dir = (Get-Location).ProviderPath
  $action = if ($rest) {
    New-ScheduledTaskAction -Execute $exe -Argument $rest -WorkingDirectory $dir
  } else {
    New-ScheduledTaskAction -Execute $exe -WorkingDirectory $dir
  }
  $user = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive -RunLevel Highest
  $set = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit 0
  $name = "trivm-$([guid]::NewGuid())"
  Register-ScheduledTask -TaskName $name -Action $action -Principal $user -Settings $set | Out-Null
  # A task unregistered before it has started never runs. Its result: 267011 not run yet, 267009
  # running, 0 done, else the program's failure or exit code.
  try {
    Start-ScheduledTask -TaskName $name
    for ($i = 0; $i -lt 100; $i++) {
      $res = (Get-ScheduledTaskInfo -TaskName $name).LastTaskResult
      if ($res -ne 267011) { break }
      Start-Sleep -Milliseconds 100
    }
  } finally { Unregister-ScheduledTask -TaskName $name -Confirm:$false }
  if ($res -eq 267011) { throw "desktop: $exe didn't start in 10 seconds" }
  if ($res -ne 267009 -and $res -ne 0) { throw ('desktop: {0} failed: 0x{1:x8}' -f $exe, $res) }
}
$global:LASTEXITCODE = 0
$global:trivm = $null
"#;

/// After the command, the exit code in `$trivm`: the last native command's, or 1 when a cmdlet
/// failed. A `return` skips it, leaving `$trivm` empty: the last native command's then.
const EPILOGUE: &str = r#"
$global:trivm = if ($?) { $LASTEXITCODE } elseif ($LASTEXITCODE) { $LASTEXITCODE } else { 1 }
"#;

/// The longest command line sshd's shell, cmd, runs.
const CMD_MAX: usize = 8191;

/// `powershell` running `script` after the prelude, as base64 of UTF-8, which no shell in between
/// unquotes (sshd's shell is cmd), decoded by `-Command`: PowerShell 5.1 writes the errors of an
/// `-EncodedCommand` as CLIXML to a stderr that isn't a console, whatever `-OutputFormat` says.
pub fn powershell(script: &str) -> io::Result<String> {
  let script = base64(format!("{PRELUDE}{script}{EPILOGUE}").as_bytes());
  let opts = "-NoLogo -NoProfile -NonInteractive -OutputFormat Text";
  let text = format!("[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{script}'))");
  // `-Command` formats its whole line at the end, which an `exit` skips: `Out-Default` writes the
  // script's output (tables too) before it. A script that doesn't parse leaves `$trivm` at 1.
  let exit = "if ($null -eq $trivm) { $trivm = $LASTEXITCODE }; exit $trivm";
  let run = format!("$trivm = 1; iex ({text}) | Out-Default; {exit}");
  let cmd = format!("powershell {opts} -Command \"{run}\"");
  if cmd.len() > CMD_MAX {
    let why = format!("the command is {} characters, past cmd's {CMD_MAX}", cmd.len());
    return Err(bad(format!("{why}: copy a script with trivm cp and run that")));
  }

  Ok(cmd)
}

/// An interactive PowerShell with the prelude.
pub fn shell() -> String {
  format!("powershell -NoLogo -NoExit -EncodedCommand {}", encode(PRELUDE))
}

fn encode(script: &str) -> String {
  let bytes: Vec<u8> = script.encode_utf16().flat_map(|x| x.to_le_bytes()).collect();
  base64(&bytes)
}

fn base64(data: &[u8]) -> String {
  const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
  for x in data.chunks(3) {
    let n =
      (x[0] as u32) << 16 | (*x.get(1).unwrap_or(&0) as u32) << 8 | *x.get(2).unwrap_or(&0) as u32;
    for i in 0..4 {
      match i <= x.len() {
        true => s.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char),
        false => s.push('='),
      }
    }
  }

  s
}

/// Waits up to 5 minutes for the desktop: Explorer running in the user's session, and the first
/// sign-in's animation gone, which covers the desktop for a minute and a half after an install.
pub const DESKTOP: &str = r#"for ($i = 0; $i -lt 150; $i++) {
  $up = Get-Process explorer -ErrorAction SilentlyContinue | Where-Object SessionId -gt 0
  if ($up -and -not (Get-Process FirstLogonAnim -ErrorAction SilentlyContinue)) { exit 0 }
  Start-Sleep 2
}
'no desktop in 5 minutes'; exit 1"#;

/// Sets the screen's mode, unless Windows draws at it, and says the one it had: Windows keeps a
/// monitor's mode by an id holding one byte of its EDID, the checksum, so a size may get another's,
/// and takes none from an EDID that names it in no first block. In the desktop's session.
pub fn screen(width: f64, height: f64) -> String {
  SCREEN.replace("WIDTH", &width.to_string()).replace("HEIGHT", &height.to_string())
}

const SCREEN: &str = r#"$want = 'WIDTHxHEIGHT'
function mode { $c = @(Get-CimInstance Win32_VideoController)[0]; '{0}x{1}' -f $c.CurrentHorizontalResolution, $c.CurrentVerticalResolution }
$was = mode
if ($was -eq $want) { exit 0 }
# DEVMODEW, 220 bytes: dmSize at 68, dmFields at 72, dmPelsWidth at 172, dmPelsHeight at 176;
# CDS_UPDATEREGISTRY keeps the mode for the monitor. The display's name a null pointer, which
# PowerShell's $null as a string isn't.
$set = @'
$u = Add-Type -Name Display -Namespace Trivm -PassThru -MemberDefinition '[DllImport("user32.dll")] public static extern int ChangeDisplaySettingsExW(IntPtr name, byte[] mode, IntPtr hwnd, int flags, IntPtr param);'
$m = New-Object byte[] 220
[BitConverter]::GetBytes([int16]220).CopyTo($m, 68)
[BitConverter]::GetBytes(0x180000).CopyTo($m, 72)
[BitConverter]::GetBytes(WIDTH).CopyTo($m, 172)
[BitConverter]::GetBytes(HEIGHT).CopyTo($m, 176)
exit $u::ChangeDisplaySettingsExW([IntPtr]::Zero, $m, [IntPtr]::Zero, 1, [IntPtr]::Zero)
'@
$enc = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($set))
desktop powershell.exe -NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand $enc
for ($i = 0; $i -lt 40 -and (mode) -ne $want; $i++) { Start-Sleep -Milliseconds 250 }
if ((mode) -ne $want) { "Windows draws at $(mode), not $want"; exit 1 }
$was"#;

// MARK: The install

/// The seed's file that the first logon runs; Setup finds `autounattend.xml` beside it.
pub const SETUP: &str = "trivm-setup.ps1";

fn component(name: &str, body: &str) -> String {
  format!(
    r#"    <component name="{name}" processorArchitecture="arm64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
{body}    </component>
"#
  )
}

/// Setup's answers: en-US, the disk wiped (EFI, MSR, Windows), Windows 11 Pro, the TPM, Secure
/// Boot and RAM checks skipped, no network needed for OOBE, the local user, OOBE's pages hidden,
/// one automatic logon that runs the setup script from the seed, whichever drive it is.
pub fn autounattend(host: &str) -> String {
  let locale = "      <InputLocale>en-US</InputLocale>\n      <SystemLocale>en-US</SystemLocale>\n      \
    <UILanguage>en-US</UILanguage>\n      <UserLocale>en-US</UserLocale>\n";
  let checks: String = ["BypassTPMCheck", "BypassSecureBootCheck", "BypassRAMCheck"]
    .iter()
    .enumerate()
    .map(|(i, x)| {
      format!(
        "        <RunSynchronousCommand wcm:action=\"add\"><Order>{}</Order><Path>reg add HKLM\\SYSTEM\\Setup\\LabConfig /v {x} /t REG_DWORD /d 1 /f</Path></RunSynchronousCommand>\n",
        i + 1
      )
    })
    .collect();
  let password =
    format!("<Password><Value>{PASSWORD}</Value><PlainText>true</PlainText></Password>");
  let setup = format!(
    "cmd /c for %d in (D E F G H I J K L M N O P) do if exist %d:\\{SETUP} powershell -NoProfile -ExecutionPolicy Bypass -File %d:\\{SETUP}"
  );

  let pe = component(
    "Microsoft-Windows-International-Core-WinPE",
    &format!("      <SetupUILanguage><UILanguage>en-US</UILanguage></SetupUILanguage>\n{locale}"),
  ) + &component(
    "Microsoft-Windows-Setup",
    &format!(
      r#"      <DiskConfiguration>
        <Disk wcm:action="add">
          <DiskID>0</DiskID>
          <WillWipeDisk>true</WillWipeDisk>
          <CreatePartitions>
            <CreatePartition wcm:action="add"><Order>1</Order><Type>EFI</Type><Size>300</Size></CreatePartition>
            <CreatePartition wcm:action="add"><Order>2</Order><Type>MSR</Type><Size>16</Size></CreatePartition>
            <CreatePartition wcm:action="add"><Order>3</Order><Type>Primary</Type><Extend>true</Extend></CreatePartition>
          </CreatePartitions>
          <ModifyPartitions>
            <ModifyPartition wcm:action="add"><Order>1</Order><PartitionID>1</PartitionID><Format>FAT32</Format><Label>System</Label></ModifyPartition>
            <ModifyPartition wcm:action="add"><Order>2</Order><PartitionID>2</PartitionID></ModifyPartition>
            <ModifyPartition wcm:action="add"><Order>3</Order><PartitionID>3</PartitionID><Format>NTFS</Format><Label>Windows</Label><Letter>C</Letter></ModifyPartition>
          </ModifyPartitions>
        </Disk>
      </DiskConfiguration>
      <ImageInstall>
        <OSImage>
          <InstallFrom><MetaData wcm:action="add"><Key>/IMAGE/NAME</Key><Value>Windows 11 Pro</Value></MetaData></InstallFrom>
          <InstallTo><DiskID>0</DiskID><PartitionID>3</PartitionID></InstallTo>
        </OSImage>
      </ImageInstall>
      <UserData>
        <ProductKey><Key>{PRO_KEY}</Key><WillShowUI>Never</WillShowUI></ProductKey>
        <AcceptEula>true</AcceptEula>
      </UserData>
      <RunSynchronous>
{checks}      </RunSynchronous>
"#
    ),
  );

  let specialize = component(
    "Microsoft-Windows-Shell-Setup",
    &format!("      <ComputerName>{host}</ComputerName>\n      <TimeZone>UTC</TimeZone>\n"),
  ) + &component(
    "Microsoft-Windows-Deployment",
    r#"      <RunSynchronous>
        <RunSynchronousCommand wcm:action="add"><Order>1</Order><Path>reg add HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\OOBE /v BypassNRO /t REG_DWORD /d 1 /f</Path></RunSynchronousCommand>
      </RunSynchronous>
"#,
  );

  let oobe = component("Microsoft-Windows-International-Core", locale)
    + &component(
      "Microsoft-Windows-Shell-Setup",
      &format!(
        r#"      <UserAccounts>
        <LocalAccounts>
          <LocalAccount wcm:action="add"><Name>{USER}</Name><DisplayName>User</DisplayName><Group>Administrators</Group>{password}</LocalAccount>
        </LocalAccounts>
      </UserAccounts>
      <AutoLogon><Enabled>true</Enabled><Username>{USER}</Username>{password}<LogonCount>1</LogonCount></AutoLogon>
      <OOBE>
        <HideEULAPage>true</HideEULAPage>
        <HideOEMRegistrationScreen>true</HideOEMRegistrationScreen>
        <HideOnlineAccountScreens>true</HideOnlineAccountScreens>
        <HideWirelessSetupInOOBE>true</HideWirelessSetupInOOBE>
        <HideLocalAccountScreen>true</HideLocalAccountScreen>
        <ProtectYourPC>3</ProtectYourPC>
      </OOBE>
      <FirstLogonCommands>
        <SynchronousCommand wcm:action="add"><Order>1</Order><CommandLine>{setup}</CommandLine></SynchronousCommand>
      </FirstLogonCommands>
"#
      ),
    );

  format!(
    r#"<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
  <settings pass="windowsPE">
{pe}  </settings>
  <settings pass="specialize">
{specialize}  </settings>
  <settings pass="oobeSystem">
{oobe}  </settings>
</unattend>
"#
  )
}

/// Run by the first logon from the seed, its log in `C:\Windows\Temp`. sshd starts last, so SSH
/// answering means the setup is done.
pub fn setup_script() -> String {
  format!(
    r#"$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$seed = $PSScriptRoot
Start-Transcript -Path C:\Windows\Temp\trivm-setup.log

pnputil /add-driver "$seed\netkvm\*.inf" /install

Start-Process msiexec -Wait -ArgumentList "/i `"$seed\openssh.msi`" /qn /norestart"
# The MSI starts sshd; SSH answering is the host's sign that this script is done.
Stop-Service sshd
$keys = 'C:\ProgramData\ssh\administrators_authorized_keys'
Copy-Item "$seed\key.pub" $keys
icacls $keys /inheritance:r /grant '*S-1-5-32-544:F' /grant '*S-1-5-18:F'
# Keys only, as on the others: the forwarded port is the Mac's other users' to reach too. The
# first of a setting counts; sshd made its config when the MSI started it.
$config = 'C:\ProgramData\ssh\sshd_config'
if (Test-Path $config) {{
  Set-Content $config (@('PasswordAuthentication no', 'KbdInteractiveAuthentication no') + (Get-Content $config))
}}
if (-not (Get-NetFirewallRule -Name trivm-ssh -ErrorAction SilentlyContinue)) {{
  New-NetFirewallRule -Name trivm-ssh -DisplayName 'OpenSSH (trivm)' -Direction Inbound -Protocol TCP -LocalPort 22 -Action Allow -Profile Any | Out-Null
}}

$logon = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon'
Set-ItemProperty $logon AutoAdminLogon '1'
Set-ItemProperty $logon DefaultUserName '{USER}'
Set-ItemProperty $logon DefaultPassword '{PASSWORD}'
Remove-ItemProperty $logon AutoLogonCount -ErrorAction SilentlyContinue

powercfg /hibernate off
foreach ($x in 'monitor-timeout-ac', 'standby-timeout-ac', 'disk-timeout-ac') {{ powercfg /change $x 0 }}
powercfg /setacvalueindex SCHEME_CURRENT SUB_BUTTONS PBUTTONACTION 3
powercfg /setactive SCHEME_CURRENT
New-Item -Force HKLM:\SOFTWARE\Policies\Microsoft\Windows\Personalization | Out-Null
Set-ItemProperty HKLM:\SOFTWARE\Policies\Microsoft\Windows\Personalization NoLockScreen 1
Set-ItemProperty 'HKCU:\Control Panel\Desktop' ScreenSaveActive '0'
New-Item -Force HKLM:\SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate\AU | Out-Null
Set-ItemProperty HKLM:\SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate\AU NoAutoUpdate 1

Set-Service sshd -StartupType Automatic
Start-Service sshd
Stop-Transcript
"#
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn lines() {
    let v = |x: &[&str]| x.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert_eq!(line(&v(&["Get-Date | Format-List"])), "Get-Date | Format-List");
    assert_eq!(line(&v(&["cmd", "/c", "it's"])), "& 'cmd' '/c' 'it''s'");
  }

  #[test]
  fn screens() {
    let script = screen(1366.0, 768.0);
    assert!(script.starts_with("$want = '1366x768'\n"));
    assert!(script.contains("GetBytes(1366).CopyTo($m, 172)\n[BitConverter]::GetBytes(768)"));
    assert!(!script.contains("WIDTH") && !script.contains("HEIGHT"));
    assert!(powershell(&script).is_ok());
  }

  #[test]
  fn encoding() {
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"f"), "Zg==");
    assert_eq!(base64(b"fo"), "Zm8=");
    assert_eq!(base64(b"foo"), "Zm9v");
    assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    // `[Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes('dir'))` in PowerShell.
    assert_eq!(encode("dir"), "ZABpAHIA");

    let cmd = powershell("exit 3").unwrap();
    let run = "-OutputFormat Text -Command \"$trivm = 1; iex ([Text.Encoding]::UTF8.GetString(";
    assert!(cmd.starts_with(&format!("powershell -NoLogo -NoProfile -NonInteractive {run}")));
    let script = base64(format!("{PRELUDE}exit 3{EPILOGUE}").as_bytes());
    let end = "| Out-Default; if ($null -eq $trivm) { $trivm = $LASTEXITCODE }; exit $trivm\"";
    assert!(cmd.ends_with(&format!("FromBase64String('{script}'))) {end}")));
    // cmd's operators only between the quotes, where it reads them as text; no `%` anywhere.
    assert_eq!(cmd.matches('"').count(), 2);
    let (head, _) = cmd.split_once('"').unwrap();
    assert!(!head.contains(['&', '|', '<', '>', '^']));
    assert!(!cmd.contains('%'));

    // A script of 3 KB fits cmd's line; one of 8 KB doesn't.
    assert!(powershell(&"x".repeat(3072)).is_ok());
    assert!(powershell(&"x".repeat(8192)).is_err());
  }

  #[test]
  fn install() {
    let xml = autounattend("windows-11");
    for x in [
      "<ComputerName>windows-11</ComputerName>",
      "<Value>Windows 11 Pro</Value>",
      "/v BypassTPMCheck /t REG_DWORD",
      "/v BypassSecureBootCheck /t REG_DWORD",
      "\\OOBE /v BypassNRO",
      "<Name>user</Name><DisplayName>User</DisplayName><Group>Administrators</Group><Password><Value>1234</Value>",
      "<LogonCount>1</LogonCount>",
      "if exist %d:\\trivm-setup.ps1 powershell",
    ] {
      assert!(xml.contains(x), "{x}");
    }

    assert_eq!(xml.matches("<component ").count(), 6);
    assert_eq!(xml.matches("<component ").count(), xml.matches("</component>").count());
    assert!(!xml.contains('\t'));

    let ps = setup_script();
    assert!(ps.contains("Set-ItemProperty $logon DefaultPassword '1234'"));
    assert!(ps.trim_end().ends_with("Start-Service sshd\nStop-Transcript"));
    // The MSI starts sshd: stopped until the end.
    assert!(ps.find("msiexec") < ps.find("Stop-Service sshd"));
    assert!(ps.contains("@('PasswordAuthentication no', 'KbdInteractiveAuthentication no')"));
  }
}
