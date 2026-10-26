<#
.SYNOPSIS
Renders one frame of the Godot example to a PNG, without a window.

.DESCRIPTION
Builds the GDExtension, copies it into the example project, and runs Godot's
headless display server with the project's capture driver. The engine draws no
frame in headless mode unless an output asks for one, so the driver registers
itself as an additional output; that is what keeps the view's frames coming.
The image is the view's own delivery frame read back on the renderer's device,
so it needs no RenderingDevice from the engine.

Prints the image path on the last line. The zero-copy probe failures the log
records are expected in headless mode: there is no engine Vulkan device to
import the frame into.

.PARAMETER Out
Where to write the PNG. Defaults to .scratch/captures/godot-<world>.png.

.PARAMETER World
The World to load: castle, bistro, sponza or nuke for the .vox files under
worlds/, sand-ball for the fixed ball, generated for a procedural World, or
empty for the one a run opens on.

.PARAMETER Frames
Frames waited after the World settles before the image is taken, so a load's
last uploads are not in the picture.

.PARAMETER Size
The capture's extent, as WIDTHxHEIGHT.

.PARAMETER Eye
The camera position, as X,Y,Z in the scene's coordinates. Needs -Target. The
fixed World types build at the lattice's bottom, far below where a run's camera
starts, so a capture of one names its own eye.

.PARAMETER Target
The point the camera looks at, as X,Y,Z in the scene's coordinates.

.PARAMETER Godot
The Godot binary to run. Defaults to $env:ATLAS_GODOT, then to the fork's
console binary. The engine needs the VulkanHooks module for the zero-copy
path, though a capture saves frames without it.

.PARAMETER Release
Builds the extension in the release profile, which is much faster at reading a
large world.

.EXAMPLE
./scripts/capture-godot.ps1

.EXAMPLE
./scripts/capture-godot.ps1 -World sand-ball -Size 1920x1080
#>
[CmdletBinding()]
param(
    [string]$Out = '',

    [ValidateSet('', 'castle', 'bistro', 'sponza', 'nuke', 'sand-ball', 'generated')]
    [string]$World = '',

    [int]$Frames = 30,

    [string]$Size = '1280x720',

    [string]$Eye = '',

    [string]$Target = '',

    [string]$Godot = '',

    [switch]$Release
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$project = Join-Path $root 'crates/atlas-rt-godot/examples/project'

if ([string]::IsNullOrEmpty($Out)) {
    $scene = if ($World -ne '') { $World } else { 'startup' }
    $cameraSuffix = if ($Eye -ne '') { '-look' } else { '' }

    $Out = Join-Path $root ".scratch/captures/godot-$scene$cameraSuffix.png"
}

$Out = [System.IO.Path]::GetFullPath($Out)
$folder = Split-Path -Parent $Out

if (!(Test-Path $folder)) {
    New-Item -ItemType Directory -Force -Path $folder | Out-Null
}

if ($Godot -eq '') {
    $Godot = $env:ATLAS_GODOT
}

if ($Godot -eq '') {
    $fork = Join-Path $env:USERPROFILE 'Desktop/dev/godot-fork/godot/bin'
    $console = Join-Path $fork 'godot.windows.editor.x86_64.console.exe'

    if (Test-Path $console) {
        $Godot = $console
    }
}

if ($Godot -eq '' -or !(Test-Path $Godot)) {
    throw 'No Godot binary: pass -Godot <path> or set ATLAS_GODOT.'
}

$profile = if ($Release) { 'release' } else { 'debug' }

cargo build -p atlas-rt-godot $(if ($Release) { '--release' }) --manifest-path (Join-Path $root 'Cargo.toml')

if ($LASTEXITCODE -ne 0) {
    throw "cargo build failed with exit code $LASTEXITCODE"
}

$built = Join-Path $root "target/$profile/atlas_rt_godot.dll"
$loaded = Join-Path $project 'lib/atlas_rt_godot.dll'

New-Item -ItemType Directory -Force -Path (Split-Path -Parent $loaded) | Out-Null

if ((Resolve-Path $built).Path -ne (Resolve-Path $loaded -ErrorAction SilentlyContinue).Path) {
    Copy-Item $built $loaded -Force
}

# The engine resolves the request after `--`, so the flags before it are the
# engine's and the ones after are the capture driver's.
$arguments = @(
    '--headless',
    '--path', $project,
    '--',
    '--capture', $Out,
    '--frames', "$Frames",
    '--size', $Size
)

if ($World -ne '') {
    $arguments += @('--world', $World)
}

if ($Eye -ne '') {
    $arguments += @('--eye', $Eye)
}

if ($Target -ne '') {
    $arguments += @('--target', $Target)
}

& $Godot @arguments

if ($LASTEXITCODE -ne 0) {
    throw "the capture failed with exit code $LASTEXITCODE"
}

Write-Output $Out
