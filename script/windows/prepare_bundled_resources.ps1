#
# Prepares bundled resources for distribution on Windows.
#
# This script copies resources that should be bundled with Mnemos into a
# destination directory. It is used by the Windows build script.
#
# Usage:
#   prepare_bundled_resources.ps1 <destination_directory>
#
# Arguments:
#   destination_directory: The directory where resources should be installed.
#                          Resources will be copied to subdirectories within
#                          this path.

Param(
    [Parameter(Mandatory = $true)]
    [String]$DestinationDir,

    [Parameter(Mandatory = $false)]
    [String]$Channel = '',

    [Parameter(Mandatory = $false)]
    [String]$CargoProfile = ''
)

$ErrorActionPreference = 'Stop'

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = (Get-Item "$ScriptDir\..\.." | Select-Object -ExpandProperty FullName)
$ResourcesSource = Join-Path $RepoRoot 'resources'

# Validate that the source resources directory exists
if (-Not (Test-Path $ResourcesSource -PathType Container)) {
    Write-Error "Resources directory not found at $ResourcesSource"
    exit 1
}

# Create the destination directory if it doesn't exist
if (-Not (Test-Path $DestinationDir)) {
    New-Item -ItemType Directory -Path $DestinationDir -Force | Out-Null
}

# Mnemos IDE: resources\bundled (Warp's bundled agent skills) is gone. Remove the skills an older
# build of this script copied into the destination, so they cannot ride along into a new bundle.
foreach ($Stale in @('skills', 'mcp_skills', '.DS_Store')) {
    $StalePath = Join-Path (Join-Path $DestinationDir 'bundled') $Stale
    if (Test-Path $StalePath) {
        Remove-Item -Path $StalePath -Recurse -Force
    }
}

if ($env:GIT_RELEASE_TAG) {
    $VersionMetadataDir = Join-Path (Join-Path $DestinationDir 'bundled') 'metadata'
    $VersionMetadataPath = Join-Path $VersionMetadataDir 'version.json'
    Write-Output "Writing bundled Mnemos version metadata to $VersionMetadataPath"
    if (-Not (Test-Path $VersionMetadataDir -PathType Container)) {
        New-Item -ItemType Directory -Path $VersionMetadataDir -Force | Out-Null
    }

    @{ mnemos_version = $env:GIT_RELEASE_TAG } |
        ConvertTo-Json |
        Set-Content -Path $VersionMetadataPath -Encoding utf8
}

# Generate third-party license attribution.
#
# Additional (non-Cargo) third-party license files to include in the output.
# When adding a new third-party component to the bundle, add its license file
# to the repo alongside the component and add an entry here.
# Cross-platform components:
# Keep this list in step with ADDITIONAL_LICENSES in script/prepare_bundled_resources.
$AdditionalLicenses = @(
    @{ Name = 'Hack Font'; License = 'MIT'; Path = 'app\assets\bundled\fonts\hack\LICENSE.md' },
    @{ Name = 'Roboto Font'; License = 'SIL Open Font License'; Path = 'app\assets\bundled\fonts\roboto\LICENSE.txt' },
    @{ Name = 'Roboto font files (their name table declares Apache-2.0, Copyright 2011 Google Inc.)'; License = 'Apache-2.0'; Path = 'licenses\Apache-2.0.txt' },
    @{ Name = 'Roboto Flex Font'; License = 'SIL Open Font License'; Path = 'licenses\roboto-flex-OFL-1.1.txt' },
    @{ Name = 'bash-preexec'; License = 'MIT'; Path = 'app\assets\bundled\bootstrap\bash-preexec-LICENSE.md' },
    @{ Name = 'rudder-sdk-rust'; License = 'MIT'; Path = 'app\src\server\telemetry\LICENSE-RUDDER-SDK-RUST.txt' },
    @{ Name = 'Windows Terminal'; License = 'MIT'; Path = 'app\assets\windows\LICENSE-WINDOWS-TERMINAL' },
    @{ Name = 'GitHub Desktop'; License = 'MIT'; Path = 'app\src\code_review\GITHUB-DESKTOP-LICENSE' },
    @{ Name = 'Command signatures and completion metadata (warp-command-signatures, warp-completion-metadata; Warp, derived from Fig)'; License = 'MIT'; Path = 'licenses\command-signatures-MIT.txt' },
    @{ Name = 'Warp workflows (warp-workflows, warp-workflows-types)'; License = 'Apache-2.0'; Path = 'licenses\warp-workflows-Apache-2.0.txt' },
    @{ Name = 'Command corrections (Warp, derived from thefuck)'; License = 'MIT'; Path = 'licenses\command-corrections-MIT.txt' },
    @{ Name = 'tree-sitter Ruby queries (Rob Rix)'; License = 'MIT'; Path = 'licenses\tree-sitter-ruby-queries-MIT.txt' },
    @{ Name = 'nvim-treesitter queries'; License = 'Apache-2.0'; Path = 'licenses\nvim-treesitter-queries-Apache-2.0.txt' },
    @{ Name = 'tree-sitter Scala queries (Max Brunsfeld and GitHub)'; License = 'MIT'; Path = 'licenses\tree-sitter-scala-queries-MIT.txt' },
    @{ Name = 'Reachability (Tony Million)'; License = 'BSD-2-Clause'; Path = 'licenses\reachability-BSD-2-Clause.txt' }
)
# Windows-only components:
$AdditionalLicenses += @(
    @{ Name = 'OpenConsole / ConPTY (Windows Terminal)'; License = 'MIT'; Path = 'app\assets\windows\LICENSE-WINDOWS-TERMINAL' },
    @{ Name = 'DirectX Shader Compiler (dxcompiler.dll, LLVM parts)'; License = 'NCSA'; Path = 'app\assets\windows\LICENSE-DXC' },
    @{ Name = 'DirectX Shader Compiler (dxcompiler.dll, Microsoft parts)'; License = 'MIT'; Path = 'licenses\directx-shader-compiler-MIT.txt' },
    @{ Name = 'DirectX Shader Compiler validator (dxil.dll, Microsoft)'; License = 'Microsoft Software License Terms'; Path = 'licenses\microsoft-dxil-LICENSE-MS.txt' },
    @{ Name = 'Microsoft Visual C++ runtime (vcruntime140.dll, vcruntime140_1.dll, msvcp140.dll)'; License = 'Microsoft Visual Studio Distributable Code terms'; Path = 'licenses\microsoft-vc-runtime.txt' }
)

# AGPL-3.0 sections 5(d) and 6: every bundle carries the license texts and a NOTICE that names
# the commit it was built from and where that commit's source is published.
Copy-Item -Path (Join-Path $RepoRoot 'LICENSE-AGPL') -Destination $DestinationDir -Force
Copy-Item -Path (Join-Path $RepoRoot 'LICENSE-MIT') -Destination $DestinationDir -Force
$SourceUrl = $env:MNEMOS_SOURCE_URL
if (-not $SourceUrl) {
    $SourceUrl = Get-Content -Path (Join-Path $RepoRoot 'script\source-url') |
        Where-Object { $_.Trim() -and -not $_.TrimStart().StartsWith('#') } |
        Select-Object -First 1
}
$SourceUrl = "$SourceUrl".Trim().TrimEnd('/')
$SourceCommit = $env:MNEMOS_GIT_COMMIT
$TreeDirty = $false
$global:LASTEXITCODE = $null
$ErrorActionPreference = 'Continue'
if (-not $SourceCommit) {
    $SourceCommit = git -C $RepoRoot rev-parse HEAD 2>$null
    if ($LASTEXITCODE -ne 0) { $SourceCommit = '' }
}
if ($SourceCommit) {
    # Untracked files count: they are not in the published source of that commit.
    # script\windows\release.ps1 reads the marker below back and refuses to publish such a build.
    $TreeDirty = [bool](git -C $RepoRoot status --porcelain 2>$null)
}
$ErrorActionPreference = 'Stop'
$Notice = @(Get-Content -Path (Join-Path $RepoRoot 'NOTICE'))
$Notice += ''
$Notice += 'Source code of this build'
$Notice += '-------------------------'
$Notice += ''
if ($SourceCommit) {
    $SourceCommit = "$SourceCommit".Trim()
    $Notice += "This copy of Mnemos IDE was built from commit $SourceCommit."
    $Notice += 'Its complete Corresponding Source is at:'
    $Notice += ''
    $Notice += "  $SourceUrl/tree/$SourceCommit"
    if ($env:GIT_RELEASE_TAG) {
        $Notice += ''
        $Notice += 'The source archive of this release, with the vendored source of every Rust'
        $Notice += "dependency, is Mnemos-source-$SourceCommit.tar.gz,"
        $Notice += 'published next to its installers.'
    }
    if ($TreeDirty) {
        $Notice += ''
        $Notice += 'This is NOT a release build: the tree had uncommitted or untracked changes when it was built.'
    }
    if ($env:MNEMOS_SOURCE_UNPUBLISHED -eq '1') {
        $Notice += ''
        $Notice += 'This is an internal TwinCurrent build: its commit was not on the public'
        $Notice += 'repository when it was built.'
    }
} else {
    $Notice += "Source repository: $SourceUrl"
}
Set-Content -Path (Join-Path $DestinationDir 'NOTICE') -Value $Notice

$LicensesOutput = Join-Path $DestinationDir 'THIRD_PARTY_LICENSES.txt'
Write-Output "Generating third-party licenses at $LicensesOutput"
# Native tools run under 'Continue' and are judged by exit code: when output is redirected, Windows
# PowerShell 5.1 turns their stderr lines into errors that 'Stop' makes fatal and that leave $? false
# even on exit code 0 (see script/windows/bundle.ps1).
$global:LASTEXITCODE = $null
$ErrorActionPreference = 'Continue'
cargo about generate --workspace --manifest-path "$RepoRoot\Cargo.toml" -c "$RepoRoot\about.toml" -o "$LicensesOutput" --fail "$RepoRoot\about.hbs"
$code = $LASTEXITCODE
$ErrorActionPreference = 'Stop'
if ($code -ne 0) {
    Write-Error 'Failed to generate third-party licenses'
    exit 1
}

# Append additional (non-Cargo) third-party licenses.
foreach ($entry in $AdditionalLicenses) {
    $LicenseFile = Join-Path $RepoRoot $entry.Path
    if (-Not (Test-Path $LicenseFile)) {
        Write-Error "License file not found: $LicenseFile"
        exit 1
    }
    Add-Content -Path $LicensesOutput -Value ''
    Add-Content -Path $LicensesOutput -Value "$($entry.Name) ($($entry.License))"
    Add-Content -Path $LicensesOutput -Value ('-' * 80)
    Get-Content -Path $LicenseFile | Add-Content -Path $LicensesOutput
    Add-Content -Path $LicensesOutput -Value ''
}

# Generate settings JSON schema unless explicitly skipped.
if ($env:SKIP_SETTINGS_SCHEMA -ne '1') {
    $SchemaOutput = Join-Path $DestinationDir 'settings_schema.json'
    Write-Output "Generating settings schema at $SchemaOutput"

    $SchemaCmd = @('run')
    if ($CargoProfile) {
        $SchemaCmd += @('--profile', $CargoProfile)
    }
    $SchemaCmd += @('--manifest-path', (Join-Path $RepoRoot 'Cargo.toml'), '--bin', 'generate_settings_schema', '--')
    if ($Channel) {
        $SchemaCmd += @('--channel', $Channel)
    }
    $SchemaCmd += $SchemaOutput

    $global:LASTEXITCODE = $null
    $ErrorActionPreference = 'Continue'
    & cargo @SchemaCmd
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($code -ne 0) {
        Write-Error 'Failed to generate settings schema'
        exit 1
    }
}

# No agent runtime is bundled, so nothing from Anthropic ships in a Mnemos
# installer. Remove an agent\ dir an older build of this script left in the
# destination: the installer packs resources\* whole.
$StaleAgentDir = Join-Path $DestinationDir 'agent'
if (Test-Path $StaleAgentDir) {
    Remove-Item -Path $StaleAgentDir -Recurse -Force
}

Write-Output "Successfully prepared bundled resources in $DestinationDir"
