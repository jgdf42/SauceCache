param([string]$Executable = "$PSScriptRoot\target\debug\saucecache-rust.exe", [switch]$About, [ValidateSet('', 'General', 'MQTT', 'About')][string]$Settings = '', [int]$WindowWidth = 0, [switch]$AddRepos, [int]$WindowHeight = 0, [switch]$TestMqtt, [switch]$RepoOptions)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class SauceCacheWindow {
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr context);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hwnd, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hwnd, int command);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr hwnd, out RECT rect);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hwnd, IntPtr insertAfter, int x, int y, int width, int height, uint flags);
}
'@
[SauceCacheWindow]::SetProcessDpiAwarenessContext([IntPtr](-4)) | Out-Null
$previewDir=Join-Path $PSScriptRoot 'docs'
New-Item -ItemType Directory -Path $previewDir -Force | Out-Null
$previewConfig=Join-Path $env:TEMP ('saucecache-smoke-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $previewConfig | Out-Null
$previousAppData=$env:APPDATA
try {
    $env:APPDATA=$previewConfig
    $previewArgs=@('--smoke-test')
    if ($About) { $previewArgs+='--preview-about' }
    if ($AddRepos) { $previewArgs+='--preview-add' }
    if ($RepoOptions) { $previewArgs+='--preview-repo-options' }
    if ($TestMqtt) { $previewArgs+='--preview-mqtt-test' }
    if ($Settings) { $previewArgs+='--preview-settings='+@('General','MQTT','About').IndexOf($Settings) }
    $process=Start-Process -FilePath $Executable -ArgumentList $previewArgs -WorkingDirectory $PSScriptRoot -PassThru -RedirectStandardError (Join-Path $previewDir 'smoke-stderr.log') -RedirectStandardOutput (Join-Path $previewDir 'smoke-stdout.log')
    $null=$process.Handle
} finally { $env:APPDATA=$previousAppData }
Start-Sleep -Seconds 3
$process.Refresh()
if ($process.HasExited) { Get-Content -LiteralPath (Join-Path $previewDir 'smoke-stderr.log'); throw "UI exited early: $($process.ExitCode)" }
$rect=New-Object SauceCacheWindow+RECT
if (-not [SauceCacheWindow]::GetWindowRect($process.MainWindowHandle,[ref]$rect)) {throw 'No application window was created'}
if ($WindowWidth -gt 0 -or $WindowHeight -gt 0) {
    $client=New-Object SauceCacheWindow+RECT
    [SauceCacheWindow]::GetClientRect($process.MainWindowHandle,[ref]$client) | Out-Null
    $scale=[SauceCacheWindow]::GetDpiForWindow($process.MainWindowHandle)/96.0
    $outerWidth=if($WindowWidth -gt 0) {[int]($WindowWidth*$scale)+($rect.Right-$rect.Left)-$client.Right} else {$rect.Right-$rect.Left}
    $outerHeight=if($WindowHeight -gt 0) {[int]($WindowHeight*$scale)+($rect.Bottom-$rect.Top)-$client.Bottom} else {$rect.Bottom-$rect.Top}
    [SauceCacheWindow]::SetWindowPos($process.MainWindowHandle,[IntPtr]::Zero,0,0,$outerWidth,$outerHeight,22) | Out-Null
    Start-Sleep -Milliseconds 400
    [SauceCacheWindow]::GetWindowRect($process.MainWindowHandle,[ref]$rect) | Out-Null
}
$bitmap=New-Object System.Drawing.Bitmap(($rect.Right-$rect.Left),($rect.Bottom-$rect.Top))
$graphics=[System.Drawing.Graphics]::FromImage($bitmap)
$hdc=$graphics.GetHdc()
try { [SauceCacheWindow]::PrintWindow($process.MainWindowHandle,$hdc,2) | Out-Null }
finally { $graphics.ReleaseHdc($hdc); $graphics.Dispose() }
$previewName=if ($About) {'about-preview.png'} else {'rust-preview.png'}
if ($AddRepos) { $previewName='add-repos-preview.png' }
if ($RepoOptions) { $previewName='repo-options-preview.png' }
if ($Settings) { $previewName='settings-'+$Settings.ToLowerInvariant()+'-preview.png' }
if ($TestMqtt) { $previewName='mqtt-test-preview.png' }
$bitmap.Save((Join-Path $previewDir $previewName),[System.Drawing.Imaging.ImageFormat]::Png)
$bitmap.Dispose()
[SauceCacheWindow]::ShowWindow($process.MainWindowHandle,6) | Out-Null
Start-Sleep -Seconds 1
if ([SauceCacheWindow]::IsWindowVisible($process.MainWindowHandle)) {throw 'Minimize did not hide the window to the tray'}
$process.WaitForExit(15000) | Out-Null
if (-not $process.HasExited -or $process.ExitCode -ne 0) {throw 'UI smoke test failed'}
Write-Output 'UI smoke test passed; minimize-to-tray verified; screenshot captured from SauceCache window only.'
