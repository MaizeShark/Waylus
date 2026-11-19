# SPDX-License-Identifier: GPL-3.0-or-later
# Copies waylus.exe and exactly the FFmpeg DLLs it loads (transitively, read with
# dumpbin /dependents) into -Out, checks that everything else it imports ships with
# Windows, and runs `waylus.exe --version` with only -Out and the Windows folders on PATH.
param(
    [Parameter(Mandatory)] [string] $Exe,
    [Parameter(Mandatory)] [string] $FfmpegBin,
    [Parameter(Mandatory)] [string] $Out
)
$ErrorActionPreference = 'Stop'

$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
$dumpbin = Get-ChildItem "$vs\VC\Tools\MSVC\*\bin\Hostx64\x64\dumpbin.exe" | Select-Object -Last 1 -ExpandProperty FullName
if (-not $dumpbin) { throw 'dumpbin.exe not found' }

function Get-Dependents([string] $File) {
    foreach ($line in (& $dumpbin /nologo /dependents $File)) {
        if ($line -match '^\s+(\S+\.dll)\s*$') { $Matches[1] }
    }
}

New-Item -ItemType Directory -Force $Out | Out-Null
Copy-Item $Exe $Out
$queue = [System.Collections.Generic.Queue[string]]::new()
$queue.Enqueue((Join-Path $Out (Split-Path $Exe -Leaf)))
$seen = @{}
$bundled = @()
$system = @()
$problems = @()
while ($queue.Count -gt 0) {
    $file = $queue.Dequeue()
    foreach ($dll in Get-Dependents $file) {
        $key = $dll.ToLowerInvariant()
        if ($seen.ContainsKey($key)) { continue }
        $seen[$key] = $true
        $src = Join-Path $FfmpegBin $dll
        if (Test-Path $src) {
            Copy-Item $src $Out
            $bundled += $dll
            $queue.Enqueue((Join-Path $Out $dll))
        } elseif ($key -match '^(vcruntime|msvcp|concrt|vccorlib|ucrtbased)') {
            # Installed on the runner by Visual Studio, not on a clean Windows.
            $problems += "$dll (Visual C++ runtime: build with +crt-static)"
        } elseif ($key -match '^(api|ext)-ms-win-' -or (Test-Path "$env:SystemRoot\System32\$dll")) {
            $system += $dll
        } else {
            $problems += "$dll (not found)"
        }
    }
}
Write-Host "bundled: $($bundled -join ' ')"
Write-Host "system:  $(($system | Sort-Object) -join ' ')"
if ($problems) { throw "unresolved imports: $($problems -join ', ')" }

# The loader resolves every import at start, so a missing DLL fails here (0xC0000135).
$oldPath = $env:PATH
try {
    $env:PATH = "$((Resolve-Path $Out).Path);$env:SystemRoot\System32;$env:SystemRoot"
    $version = & (Join-Path $Out (Split-Path $Exe -Leaf)) --version
    if ($LASTEXITCODE -ne 0) { throw "waylus.exe --version failed with exit code $LASTEXITCODE" }
    Write-Host "smoke test: $version"
} finally {
    $env:PATH = $oldPath
}
