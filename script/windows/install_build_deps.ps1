#!/usr/bin/env powershell
#
# Install all dependencies required to build Mnemos on Windows.

# Install Rust + cargo.
bash (((Get-Location).path) + '\script\install_rust')

# Install various build-time dependencies through cargo.
bash (((Get-Location).path) + '\script\install_cargo_build_deps')
