# Runs the unit tests with the test binary Authenticode-signed BEFORE its
# first execution. Unsigned fresh-hash binaries in target\debug\deps trip
# Kaspersky File AV / KSN on this machine (the Trusted Applications rules
# only cover the two release paths); signing collapses the heuristic signal
# the same way it does for release builds. Usage:
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\test.ps1 [testname-filter]
param([string]$Filter = "")

# NOT "Stop": native tools (cargo, signtool) report progress on stderr, which
# PowerShell 5.1 wraps in ErrorRecords when redirected; exit codes are checked
# explicitly instead.
$ErrorActionPreference = "Continue"
$repo = Split-Path -Parent $PSScriptRoot
$cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"
$manifest = Join-Path $repo "Cargo.toml"

# Locate signtool (any Windows Kits version).
$signtool = Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin\10.0.*\x64\signtool.exe" -ErrorAction SilentlyContinue |
    Sort-Object FullName -Descending | Select-Object -First 1 -ExpandProperty FullName

# Build without running; parse the produced executable path from JSON output.
$msgs = & $cargo test --no-run --manifest-path $manifest --message-format=json
if ($LASTEXITCODE -ne 0) {
    # Re-run humanly for a readable compile error.
    & $cargo test --no-run --manifest-path $manifest
    exit $LASTEXITCODE
}
$exe = $msgs | ForEach-Object { $_ | ConvertFrom-Json } |
    Where-Object { $_.reason -eq "compiler-artifact" -and $_.profile.test -and $_.executable } |
    Select-Object -Last 1 -ExpandProperty executable
if (-not $exe) { Write-Error "could not determine test executable path"; exit 1 }

# Sign before first execution. AV may briefly hold the fresh file; retry.
if ($signtool) {
    $signed = $false
    foreach ($i in 1..20) {
        & $signtool sign /n "WinThemeSwitcher Self-Signed" /fd SHA256 $exe *> $null
        if ($LASTEXITCODE -eq 0) { $signed = $true; break }
        Start-Sleep -Milliseconds 500
    }
    if (-not $signed) { Write-Warning "test binary could not be signed (locked or cert missing) - running unsigned" }
}

if ($Filter) { & $exe $Filter } else { & $exe }
exit $LASTEXITCODE
