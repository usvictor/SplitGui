$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$exe = Join-Path $PSScriptRoot '../package/TailscaleSplitGUI.exe'
# Exercise the elevated entry point with a deliberately rejected config. This
# must fail before Tailscale/DNS/routes are touched and preserve the real error.
$config = Join-Path $env:TEMP 'SplitGUI 中文 路径.json'
'{"version":2,"domestic_domains":["printer.local"],"domestic_dns":["223.5.5.5"],"china_routes":[]}' | Set-Content -LiteralPath $config -Encoding utf8
$request = [Guid]::NewGuid().ToString('N')
$operation = Start-Process -FilePath $exe -ArgumentList "admin install $request `"$config`"" -Wait -PassThru -RedirectStandardError (Join-Path $PSScriptRoot '../dist/admin-stderr.txt')
if ($operation.ExitCode -ne 1) { throw 'Rejected configuration did not fail as expected' }
$reportPath = Join-Path ([Environment]::GetFolderPath('CommonApplicationData')) 'TailscaleSplitGUI-v2/admin-result.json'
$report = Get-Content -LiteralPath $reportPath -Raw -Encoding utf8 | ConvertFrom-Json
if ($report.request -ne $request -or $report.error -notmatch '不能接管局域网') { throw "Actual administrator error was lost: $($report.error)" }
if ($report.error.Contains([char]0xfffd)) { throw 'Administrator error contains replacement characters' }
Remove-Item -LiteralPath $config
Write-Output 'GUI startup and Chinese administrator error round-trip passed'

$stderr = Join-Path $PSScriptRoot '../dist/gui-stderr.txt'
$stdout = Join-Path $PSScriptRoot '../dist/gui-stdout.txt'
$p = Start-Process -FilePath $exe -PassThru -RedirectStandardError $stderr -RedirectStandardOutput $stdout
try {
    Start-Sleep -Seconds 8
    $p.Refresh()
    if ($p.HasExited) { Get-Content $stderr -Encoding utf8; Get-Content $stdout -Encoding utf8; throw 'GUI exited during startup' }
    if ($p.MainWindowHandle -eq 0) { throw 'GUI did not create a window' }
    $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $image = New-Object System.Drawing.Bitmap($bounds.Width, $bounds.Height)
    $graphics = [System.Drawing.Graphics]::FromImage($image)
    try {
        $graphics.CopyFromScreen($bounds.Location, [System.Drawing.Point]::Empty, $bounds.Size)
        $image.Save((Join-Path $PSScriptRoot '../dist/Windows-GUI.png'))
    } finally { $graphics.Dispose(); $image.Dispose() }
} finally {
    if (-not $p.HasExited) { $p.CloseMainWindow() | Out-Null; if (-not $p.WaitForExit(5000)) { $p.Kill() } }
}

