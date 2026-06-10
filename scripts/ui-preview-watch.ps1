# SPDX-License-Identifier: Apache-2.0
# Hot-reload loop for the origin UI/UX preview.
#
# Watches the theme/chrome sources and re-runs `origin-ui-preview` whenever
# one of them changes. Zero extra tooling required (no cargo-watch/bacon).
#
# Usage:
#   ./scripts/ui-preview-watch.ps1                 # all themes, full preview
#   ./scripts/ui-preview-watch.ps1 dark            # one theme
#   ./scripts/ui-preview-watch.ps1 --transcript    # transcript only
#
# Any arguments are forwarded to origin-ui-preview verbatim.

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot

$watched = @(
    Join-Path $root 'crates/origin-cli/src/theme.rs'
    Join-Path $root 'crates/origin-cli/src/ansi.rs'
    Join-Path $root 'crates/origin-ui-preview/src/main.rs'
)

function Get-Stamp {
    ($watched | ForEach-Object {
        if (Test-Path $_) { (Get-Item $_).LastWriteTimeUtc.Ticks } else { 0 }
    }) -join '|'
}

Write-Host "watching:" -ForegroundColor DarkGray
$watched | ForEach-Object { Write-Host "  $_" -ForegroundColor DarkGray }
Write-Host "Ctrl+C to stop.`n" -ForegroundColor DarkGray

$last = ''
while ($true) {
    $stamp = Get-Stamp
    if ($stamp -ne $last) {
        $last = $stamp
        # Build first so a compile error doesn't clear the previous render.
        cargo build -q -p origin-ui-preview 2>&1 | Out-String | ForEach-Object {
            if ($_ -match '\S') { Write-Host $_ -ForegroundColor Red }
        }
        if ($LASTEXITCODE -eq 0) {
            cargo run -q -p origin-ui-preview -- @args
            Write-Host "`n[ui-preview] rendered $(Get-Date -Format 'HH:mm:ss') — edit theme.rs/ansi.rs to re-render" -ForegroundColor DarkGray
        }
    }
    Start-Sleep -Milliseconds 300
}
