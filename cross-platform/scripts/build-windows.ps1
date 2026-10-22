$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
Push-Location $project
try {
    cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'Rust build failed' }
    New-Item -ItemType Directory -Force dist, package | Out-Null
    Copy-Item target/release/tailscale-splitgui.exe package/TailscaleSplitGUI.exe -Force
    Copy-Item README.md package/README.md -Force
    Copy-Item assets/NotoSansSC-OFL.txt package/FONT-LICENSE.txt -Force
    Compress-Archive -Path package/* -DestinationPath dist/TailscaleSplitGUI-v2-Windows-x64.zip -Force
} finally { Pop-Location }
