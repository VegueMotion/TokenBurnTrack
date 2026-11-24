$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.*)"').Matches[0].Groups[1].Value
$name = "TokenBurnTrack-$version-windows-x64"

cargo build --release --locked -p tbt-app -p tbt-daemon
if ($LASTEXITCODE) { exit $LASTEXITCODE }

$stage = "target\dist\$name"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage, dist | Out-Null
Copy-Item target\release\tbt-app.exe, target\release\tbt-daemon.exe, LICENSE, README.md $stage
Compress-Archive -Force -Path "$stage\*" -DestinationPath "dist\$name.zip"
Write-Output "dist\$name.zip"
