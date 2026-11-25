$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.*)"').Matches[0].Groups[1].Value
$suffix = "$version-windows-x64"

cargo build --release --locked -p tbt-app -p tbt-daemon
if ($LASTEXITCODE) { exit $LASTEXITCODE }

New-Item -ItemType Directory -Force dist | Out-Null
$app = "dist\TokenBurnTrack-$suffix.exe"
$daemon = "dist\tbt-daemon-$suffix.exe"
Copy-Item -Force target\release\tbt-app.exe $app
Copy-Item -Force target\release\tbt-daemon.exe $daemon
Write-Output $app $daemon
