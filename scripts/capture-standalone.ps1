<#
.SYNOPSIS
Renders one frame of the standalone app to a PNG, without a window.

.DESCRIPTION
Builds the atlas-rt binary, runs it with --capture on a device with no
surface, and prints the image path on the last line. Every flag the binary
takes for choosing a World is passed through, so a capture names the same
World a run would open on.

The image is the frame's linear radiance through the ACES fit and gamma the
Godot example's composite shader applies, so features read the same in both
captures.

.PARAMETER Out
Where to write the PNG. Defaults to .scratch/captures/standalone-<scene>.png.

.PARAMETER World
A .vox path under crates/atlas-rt/assets/, such as test/matl-alpha.vox.

.PARAMETER WorldType
A generated World: terrain, sand-ball or raining.

.PARAMETER Seed
The Seed a Generated World is built from.

.PARAMETER Extent
Bounds a Generated World's ground to an Extent by Extent square.

.PARAMETER Ticks
Simulation ticks to advance before the frame is drawn. The sim runs as fast
as it can, so this costs wall time only where a tick is heavy.

.PARAMETER Fly
Leaves the camera at the free camera's default pose instead of following the
simulated player.

.PARAMETER Eye
The camera position, as X,Y,Z in voxel coordinates. Needs -Target, and replaces
the camera a run would use: the scenes the fixed World types build sit at the
bottom of the lattice, far below the free camera's default pose.

.PARAMETER Target
The point the camera looks at, as X,Y,Z in voxel coordinates.

.PARAMETER NoSim
Runs no simulation thread, so the capture is the World as built.

.PARAMETER Size
The capture's extent, as WIDTHxHEIGHT.

.PARAMETER Release
Builds and runs the release profile, which is much faster at generating a
World and at a heavy tick.

.EXAMPLE
./scripts/capture-standalone.ps1 -WorldType sand-ball -Ticks 130 -Fly

.EXAMPLE
./scripts/capture-standalone.ps1 -Seed 42 -Extent 512 -Size 1920x1080
#>
[CmdletBinding()]
param(
    [string]$Out = '',

    [string]$World = '',

    [ValidateSet('', 'terrain', 'sand-ball', 'raining')]
    [string]$WorldType = '',

    [string]$Seed = '',

    [string]$Extent = '',

    [int]$Ticks = 0,

    [string]$Eye = '',

    [string]$Target = '',

    [switch]$Fly,

    [switch]$NoSim,

    [string]$Size = '1280x720',

    [switch]$Release
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot

if ([string]::IsNullOrEmpty($Out)) {
    $scene = if ($WorldType -ne '') { $WorldType }
        elseif ($World -ne '') { [System.IO.Path]::GetFileNameWithoutExtension($World) }
        elseif ($Seed -ne '') { "seed-$Seed" }
        else { 'random' }

    # PowerShell variable names are case-insensitive, so a variable named $ticks
    # beside the $Ticks parameter would be the same variable.
    $tickSuffix = if ($Ticks -gt 0) { "-$($Ticks)t" } else { '' }
    $cameraSuffix = if ($Eye -ne '') { '-look' } else { '' }

    $Out = Join-Path $root ".scratch/captures/standalone-$scene$tickSuffix$cameraSuffix.png"
}

$Out = [System.IO.Path]::GetFullPath($Out)
$folder = Split-Path -Parent $Out

if (!(Test-Path $folder)) {
    New-Item -ItemType Directory -Force -Path $folder | Out-Null
}

$profile = if ($Release) { 'release' } else { 'debug' }

cargo build -p atlas-rt $(if ($Release) { '--release' }) --manifest-path (Join-Path $root 'Cargo.toml')

if ($LASTEXITCODE -ne 0) {
    throw "cargo build failed with exit code $LASTEXITCODE"
}

$arguments = @('--capture', $Out, '--size', $Size, '--ticks', "$Ticks")

if ($World -ne '') { $arguments += @('--world', $World) }
if ($WorldType -ne '') { $arguments += @('--world-type', $WorldType) }
if ($Seed -ne '') { $arguments += @('--seed', $Seed) }
if ($Extent -ne '') { $arguments += @('--extent', $Extent) }
if ($Eye -ne '') { $arguments += @('--eye', $Eye) }
if ($Target -ne '') { $arguments += @('--target', $Target) }
if ($Fly) { $arguments += '--fly' }
if ($NoSim) { $arguments += '--no-sim' }

$exe = Join-Path $root "target/$profile/atlas-rt.exe"

# From the repository root, because --world resolves its path from there.
Push-Location $root

try {
    & $exe @arguments
} finally {
    Pop-Location
}

if ($LASTEXITCODE -ne 0) {
    throw "the capture failed with exit code $LASTEXITCODE"
}

Write-Output $Out


