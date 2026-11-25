param([Parameter(Mandatory=$true)][string]$Executable,[int]$Seconds=20,[string]$Output='idle-memory.json',[switch]$Visible)
$ErrorActionPreference='Stop'
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class IdleWindow {
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hwnd,int command);
}
'@
$fixture=Join-Path $env:TEMP ('saucecache-idle-'+[guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $fixture | Out-Null
$previous=$env:APPDATA
try {
    $env:APPDATA=$fixture
    $process=Start-Process -FilePath $Executable -WorkingDirectory $PSScriptRoot -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $fixture 'stderr.log') -RedirectStandardOutput (Join-Path $fixture 'stdout.log')
    $null=$process.Handle
} finally { $env:APPDATA=$previous }
$samples=@()
try {
    Start-Sleep -Seconds 2
    $process.Refresh()
    if(-not $Visible) { [IdleWindow]::ShowWindow($process.MainWindowHandle,6) | Out-Null }
    for($second=0;$second -le $Seconds;$second++) {
        $process.Refresh()
        if($process.HasExited) { throw "Idle process exited with $($process.ExitCode); see $fixture" }
        $samples += [pscustomobject]@{Second=$second;PrivateBytes=$process.PrivateMemorySize64;WorkingSet=$process.WorkingSet64}
        if($second -lt $Seconds) { Start-Sleep -Seconds 1 }
    }
} finally {
    $process.Refresh()
    if(-not $process.HasExited) { Stop-Process -Id $process.Id -Force; $process.WaitForExit() }
}
$samples | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $PSScriptRoot $Output) -Encoding UTF8
$start=$samples[0].PrivateBytes; $end=$samples[-1].PrivateBytes
Write-Output "Private memory: $([math]::Round($start/1MB,1)) MB -> $([math]::Round($end/1MB,1)) MB over $Seconds seconds. Growth $([math]::Round(($end-$start)/1MB,1)) MB."
