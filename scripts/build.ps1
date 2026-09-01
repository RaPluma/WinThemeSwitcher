# Build a release exe, Authenticode-sign it with RFC 3161 countersignature,
# and copy it to the deploy path. Identical shape to scripts\test.ps1 so both
# wrappers behave the same on AV-lock retries and signtool discovery.
#
# Why this exists: bare `cargo build --release` produces a fresh-hash
# unsigned Windows PE. Kaspersky KSN flags first-seen unsigned hashes with
# VHO:Trojan.Win32.Convagent.gen on this machine, and the only mitigation
# is to sign the binary before its first execution. The signing step must
# run AFTER cargo produces the exe and BEFORE anything executes it (this
# script only invokes signtool + Copy-Item, no exec).
#
# Usage:
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build.ps1
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build.ps1 -SkipCopy
#
# Parameters:
#   -SkipCopy   build + sign in place, don't overwrite the deployed exe at
#               C:\Tools\WinThemeSwitcher\win-theme-switcher.exe (useful when
#               verifying a build without disturbing the running install).
#
# Exit codes:
#   0  build + sign + (optional) copy all succeeded
#   1  cargo build failed
#   2  produced exe path could not be determined from cargo output
#   3  signtool not found (caller should install Windows SDK)
#   4  signtool never succeeded (AV holding the file or cert missing)
#   5  deploy directory missing (install previous version first)
param([switch]$SkipCopy)

# PowerShell 5.1 wraps native stderr in ErrorRecords when redirected; check
# $LASTEXITCODE explicitly instead of using Stop on $ErrorActionPreference.
$ErrorActionPreference = "Continue"
$repo = Split-Path -Parent $PSScriptRoot
$cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"
$manifest = Join-Path $repo "Cargo.toml"

# Locate signtool (any Windows Kits version).
$signtool = Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin\10.0.*\x64\signtool.exe" -ErrorAction SilentlyContinue |
    Sort-Object FullName -Descending | Select-Object -First 1 -ExpandProperty FullName
if (-not $signtool) {
    Write-Error "signtool.exe not found under C:\Program Files (x86)\Windows Kits\10\bin\10.0.*\x64\ -- install Windows SDK"
    exit 3
}

# Build with --message-format=json so we can locate the produced exe
# programmatically (avoids hardcoding target\release\win-theme-switcher.exe).
$msgs = & $cargo build --release --manifest-path $manifest --message-format=json
if ($LASTEXITCODE -ne 0) {
    # Re-run humanly for a readable compile error.
    & $cargo build --release --manifest-path $manifest
    exit $LASTEXITCODE
}
$exe = $msgs | ForEach-Object { $_ | ConvertFrom-Json } |
    Where-Object { $_.reason -eq "compiler-artifact" -and -not $_.profile.test -and $_.executable } |
    Select-Object -Last 1 -ExpandProperty executable
if (-not $exe) {
    Write-Error "could not determine release executable path from cargo output"
    exit 2
}

# Sign before any other process touches the exe. /tr + /td request an RFC
# 3161 timestamp from DigiCert -- without it, the Authenticode signature
# dies when the cert expires in 2036. AV may briefly hold the fresh file;
# retry up to 20 times (~10 s of wall time, plenty for the on-access
# scanner to release its handle).
$signed = $false
for ($i = 1; $i -le 20; $i++) {
    & $signtool sign /n "WinThemeSwitcher Self-Signed" /fd SHA256 `
        /tr http://timestamp.digicert.com /td SHA256 $exe *> $null
    if ($LASTEXITCODE -eq 0) { $signed = $true; break }
    Start-Sleep -Milliseconds 500
}
if (-not $signed) {
    Write-Warning "release exe could not be signed (locked or cert missing) -- refusing to copy"
    exit 4
}

Write-Output "signed: $exe"

if ($SkipCopy) {
    Write-Output "SkipCopy set -- leaving deployed binary at C:\Tools\WinThemeSwitcher\ untouched"
    exit 0
}

# Deploy. Stop any running instance first so Copy-Item does not trip on a
# locked file, then re-launch after copy. Both steps are idempotent.
$deployDir = "C:\Tools\WinThemeSwitcher"
$deployExe = Join-Path $deployDir "win-theme-switcher.exe"
if (-not (Test-Path $deployDir)) {
    Write-Error "deploy directory $deployDir does not exist -- install the previous version first"
    exit 5
}
Get-Process -Name win-theme-switcher -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 200
Copy-Item -Path $exe -Destination $deployExe -Force
Write-Output "deployed: $deployExe"
exit 0
