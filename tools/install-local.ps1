# Builds guard.exe and harness.exe and puts them where the plugin looks for them (bin/).
# Use it when the plugin is installed from a clone or from source: the repository does not
# track plugin/bin, only the installer and the release archives carry the binaries. Without
# bin/guard.exe the PreToolUse hook fails on every tool call. Run it again after a plugin update.
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
Push-Location $repo
try {
    cargo build --release -p guard -p cli
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
} finally { Pop-Location }

$targets = @(Join-Path $repo 'plugin\bin')
$cache = Join-Path $env:USERPROFILE '.claude\plugins\cache\rolo-loop\rolo-loop'
if (Test-Path $cache) {
    $targets += Get-ChildItem $cache -Directory | ForEach-Object { Join-Path $_.FullName 'bin' }
}
foreach ($dir in $targets) {
    New-Item -ItemType Directory -Force $dir | Out-Null
    foreach ($name in 'guard.exe', 'harness.exe') {
        Copy-Item (Join-Path $repo "target\release\$name") $dir -Force
    }
    Write-Host "guard.exe, harness.exe -> $dir"
}
