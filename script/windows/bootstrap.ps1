#!/usr/bin/env powershell

$ErrorActionPreference = 'Stop'

# Git for Windows can be installed system-wide (Program Files) or per-user (LOCALAPPDATA\Programs\Git).
$gitBinCandidates = @(
    "$env:PROGRAMFILES\Git\bin",
    "$env:LOCALAPPDATA\Programs\Git\bin"
)
$gitBinDir = $gitBinCandidates | Where-Object { Test-Path -PathType Container $_ } | Select-Object -First 1
if (-not $gitBinDir) {
    Write-Error 'Git for Windows is required. Please install it at:'
    Write-Error 'https://gitforwindows.org/'
    exit 1
}

# The MSVC linker and Windows SDK, which the x86_64-pc-windows-msvc Rust toolchain needs. Install
# them before Rust so rustup-init finds them.
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$hasVcTools = $false
if (Test-Path $vswhere) {
    $hasVcTools = [bool](& $vswhere -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -latest -property installationPath)
}
if (-not $hasVcTools) {
    Write-Output 'Installing the Visual Studio 2022 Build Tools (C++ workload)...'
    winget install -e --id Microsoft.VisualStudio.2022.BuildTools --override '--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'
}

if (-not (Get-Command -Name cargo -Type Application -ErrorAction SilentlyContinue)) {
    Write-Output 'Installing rust...'
    Invoke-WebRequest -Uri 'https://win.rustup.rs/x86_64' -OutFile "$env:Temp\rustup-init.exe"
    & "$env:Temp\rustup-init.exe"
    Write-Output 'Please start a new terminal session so that cargo is in your PATH'
    exit 1
}

# A bash executable should come with Git for Windows
& "$gitBinDir\bash.exe" "$PWD\script\install_cargo_test_deps"

# Needed in wasm compilation for parsing the version of wasm-bindgen
winget install jqlang.jq

# CMake is needed to build some dependencies, e.g.: sentry-contrib-native.
winget install -e --id Kitware.CMake

# protoc, for crates/remote_server's build script (prost-build).
winget install -e --id Google.Protobuf

# The ConPTY debug symbols (*.pdb, see .gitattributes) are LFS objects. Git for Windows ships
# git-lfs; this only turns it on for the user.
& git lfs install

# We use InnoSetup to build our release bundle installer.
winget install -e --id JRSoftware.InnoSetup

# Mnemos IDE: upstream installed the Google Cloud SDK and logged in here for SSH integration
# tests that tunnel into Warp's GCP project. Those tests are opt-in now, so no gcloud.

# The build's paths are long (the release scripts use the short `rlto` profile name for this
# reason). Enabling long paths needs an elevated shell, so only check and explain.
$longPaths = (Get-ItemProperty -Path 'HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem' -Name LongPathsEnabled -ErrorAction SilentlyContinue).LongPathsEnabled
if ($longPaths -ne 1) {
    Write-Output 'Note: Windows long paths are off. From an elevated PowerShell, run:'
    Write-Output "  Set-ItemProperty -Path 'HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem' -Name LongPathsEnabled -Value 1"
}
if ((& git config --global --get core.longpaths) -ne 'true') {
    Write-Output 'Enabling git core.longpaths for this user.'
    & git config --global core.longpaths true
}
