#!/usr/bin/env powershell
#
# Bundle the application for release.

Param (
    # Build dev bundles by default.
    [Switch]$DEBUG_BUILD = $False,

    [Alias('check-only')]
    [Switch]$CHECK_ONLY,

    [ValidateSet('local', 'dev', 'preview', 'stable')]
    [String]$CHANNEL = 'dev',

    [Alias('release-tag')]
    [String]$RELEASE_TAG = '',
    # Mnemos IDE: no crash_reporting by default; builds have no Sentry DSN, so
    # the feature only compiled in the Sentry SDK. Pass it explicitly to opt in.
    [String]$FEATURES = 'release_bundle,gui',

    # Builds only the Mnemos binary, skips the installer.
    [Switch]$SKIP_BUILD_INSTALLER = $False,
    # Builds only the installer, skips the Mnemos binary. Use this if the Mnemos
    # binary has already been built.
    [Switch]$SKIP_BUILD_BINARY = $False,

    [ValidateSet('x64', 'arm64')]
    [String]$ARCH = '',

    # A signtool command for Inno Setup to sign the setup engine and uninstaller.
    # Uses $f as the file placeholder, e.g.:
    #   'signtool.exe sign /fd SHA256 ... $f'
    # When empty, the installer is built without signing.
    [Alias('sign-tool-cmd')]
    [String]$SIGN_TOOL_CMD = ''
)

if ($RELEASE_TAG) {
    $env:GIT_RELEASE_TAG = $RELEASE_TAG
}

# Use provided ARCH parameter if set, otherwise detect from system
if (-not $ARCH) {
    if ($env:PROCESSOR_ARCHITECTURE -eq 'AMD64') {
        $ARCH = 'x64'
    } elseif ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') {
        $ARCH = 'arm64'
    } else {
        throw "Unsupported processor architecture: $env:PROCESSOR_ARCHITECTURE"
    }
}

if ($ARCH -eq 'arm64') {
    $FILE_ENDING = 'Setup-arm64'
    $PLATFORM_TARGET = 'aarch64-pc-windows-msvc'
} else {
    # If x64, then we just use the filename "MnemosSetup.exe" for example
    $FILE_ENDING = 'Setup'
    $PLATFORM_TARGET = 'x86_64-pc-windows-msvc'
}

$ErrorActionPreference = 'Stop'

$WORKSPACE_ROOT_DIR = $(Get-Location).Path
$CARGO_TARGET_DIR = $WORKSPACE_ROOT_DIR + '\target'
$WINDOWS_INSTALLER_DIR = $WORKSPACE_ROOT_DIR + '\script\windows'

if ($DEBUG_BUILD) {
    $CARGO_PROFILE = 'dev'
} elseif (("$CHANNEL" -eq 'local') -or ("$CHANNEL" -eq 'dev')) {
    # For dev bundles, we want to enable debug assertions to
    # catch violations that would otherwise silently pass in
    # a normal release build (e.g. in stable).
    $CARGO_PROFILE = 'rltoda'
} else {
    $CARGO_PROFILE = 'rlto'
}

if ($CARGO_PROFILE -eq 'dev') {
    $CARGO_TARGET_OUTPUT_DIR = "$CARGO_TARGET_DIR" + '\' + $PLATFORM_TARGET + '\debug'
} else {
    $CARGO_TARGET_OUTPUT_DIR = "$CARGO_TARGET_DIR" + '\' + $PLATFORM_TARGET + '\' + "$CARGO_PROFILE"
}
$BUNDLE_ID = "dev.mnemos.$app_name"

# Update parameters based on the target release channel.
#
# APP_NAME here must match the value used in Rust as the
# application name; see app/src/channel.rs.
#
# MNEMOS_BIN is the name of the binary produced by cargo;
# BINARY_NAME is the desired name of the binary in the final package.
if ("$CHANNEL" -eq 'local') {
    $MNEMOS_BIN = 'mnemos'
    $BINARY_NAME = 'mnemos.exe'
    $APP_NAME = 'MnemosLocal'
    $FEATURES = "$FEATURES,nld_improvements"
} elseif ("$CHANNEL" -eq 'dev') {
    $MNEMOS_BIN = 'dev'
    $BINARY_NAME = 'dev.exe'
    $APP_NAME = 'MnemosDev'
    $FEATURES = "$FEATURES,agent_mode_debug,nld_improvements"
} elseif ("$CHANNEL" -eq 'preview') {
    $MNEMOS_BIN = 'preview'
    $BINARY_NAME = 'preview.exe'
    $APP_NAME = 'MnemosPreview'
    $FEATURES = "$FEATURES,preview_channel,nld_improvements"
} elseif ("$CHANNEL" -eq 'stable') {
    $MNEMOS_BIN = 'mnemos'
    $BINARY_NAME = 'mnemos.exe'
    $APP_NAME = 'Mnemos'
    # TODO(vorporeal): Remove this once we get tests passing with this default enabled.
    $FEATURES = "$FEATURES,nld_improvements"
}
# Mnemos IDE: the separate `oss` channel (mnemos-oss) is retired. A source build of the stable
# channel carries no telemetry, crash reporting or updater; see PACKAGING.md.

$BINARY_PATH = "$CARGO_TARGET_OUTPUT_DIR\$BINARY_NAME"
$BUNDLE_ID = "dev.mnemos.$APP_NAME"
$INSTALLER_OUTPUT_DIR = "$WINDOWS_INSTALLER_DIR\Output"
$INSTALLER_NAME = "$($APP_NAME)$($FILE_ENDING)"
$INSTALLER_PATH = "$($INSTALLER_OUTPUT_DIR)\$($INSTALLER_NAME).exe"
$PDB_PATH = "$CARGO_TARGET_OUTPUT_DIR\$MNEMOS_BIN.pdb"

# The CARGO_FULL_PROFILE environment variable is read by the `cargo` build
# script (`app/build.rs`) to determine where to place `conpty.dll`.
if ($DEBUG_BUILD) {
    $env:CARGO_FULL_PROFILE = 'debug'
} else {
    $env:CARGO_FULL_PROFILE = $CARGO_PROFILE
}

# Native tools (cargo, ISCC) run under ErrorActionPreference 'Continue' and are judged by exit code
# alone. When output is redirected, Windows PowerShell 5.1 wraps every stderr line of a native
# command in an ErrorRecord (NativeCommandError): under 'Stop' the first one, cargo's "Compiling
# ..." progress line, kills the script seconds in, and $? reads false even on exit code 0.
# LASTEXITCODE is cleared first so a tool that never ran reads as a failure, not a stale success.
# Failures are reported with Write-Output: under 'Stop', Write-Error would throw before `exit 1`.

# If we only want to check that compilation will succeed, perform the checks
# then exit.  We use this script to invoke `cargo check` to ensure that we are
# using the same feature flags and profile that we would be using in production.
if ($CHECK_ONLY) {
    $global:LASTEXITCODE = $null
    $ErrorActionPreference = 'Continue'
    cargo check -p mnemos --profile "$CARGO_PROFILE" --bin "$MNEMOS_BIN" --features "$FEATURES" --target $PLATFORM_TARGET
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($code -ne 0) {
        Write-Output "Failed to verify Mnemos $MNEMOS_BIN compilation with profile $CARGO_PROFILE (exit code $code)"
        exit 1
    }
    exit 0
}

if (-Not $SKIP_BUILD_BINARY) {
    Write-Output "Building Mnemos for channel $CHANNEL and bundle id $BUNDLE_ID"
    $env:CARGO_BIN_NAME = $CHANNEL
    $env:MNEMOS_APP_NAME = $APP_NAME
    $global:LASTEXITCODE = $null
    $ErrorActionPreference = 'Continue'
    cargo build -p mnemos --profile "$CARGO_PROFILE" --bin "$MNEMOS_BIN" --features "$FEATURES" --target $PLATFORM_TARGET
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($code -ne 0) {
        Write-Output "Failed to build Mnemos $MNEMOS_BIN binary with profile $CARGO_PROFILE (exit code $code)"
        exit 1
    }

    # If we desire an executable name different from the cargo bin, rename it.
    if ("$MNEMOS_BIN.exe" -ne $BINARY_NAME) {
        $binarySource = "$CARGO_TARGET_OUTPUT_DIR\$MNEMOS_BIN.exe"
        Write-Output "Renaming executable $MNEMOS_BIN.exe to $BINARY_NAME"
        Move-Item -Path "$binarySource" -Destination "$BINARY_PATH" -Force
    }
}

if ($SKIP_BUILD_INSTALLER) {
    # If this is being run within a GitHub action, set an output variable with the
    # location of the binary so it can be referenced by subsequent actions.
    if ($env:GITHUB_ACTIONS -eq 'true') {
        Write-Output '::echo::on'
        "target_profile_dir=$CARGO_TARGET_OUTPUT_DIR" >> "$env:GITHUB_OUTPUT"
        "binary_path=$BINARY_PATH" >> "$env:GITHUB_OUTPUT"
        Write-Output '::echo::off'
    }
    exit 0
}

Write-Output "Built for $ARCH with executable at $BINARY_PATH"

# Prepare bundled resources
$BUNDLED_RESOURCES_DIR = "$CARGO_TARGET_OUTPUT_DIR\resources"
Write-Output "Preparing bundled resources..."
# A script's `exit 1` sets LASTEXITCODE; $? after a script call can be false merely because it
# wrote an error record. Terminating errors inside it propagate on their own.
$global:LASTEXITCODE = 0
& "$WINDOWS_INSTALLER_DIR\prepare_bundled_resources.ps1" -DestinationDir "$BUNDLED_RESOURCES_DIR" -Channel "$CHANNEL" -CargoProfile "$CARGO_PROFILE"
if ($LASTEXITCODE -ne 0) {
    Write-Output "Failed to prepare bundled resources (exit code $LASTEXITCODE)"
    exit 1
}

# Builds with the browser pane downloaded the WebView2 bootstrapper into the
# staging tree. Nothing needs WebView2 now, and the installer packs resources\*
# whole, so drop a copy an older build left behind.
$StaleWebView2Bootstrapper = Join-Path $BUNDLED_RESOURCES_DIR 'MicrosoftEdgeWebview2Setup.exe'
if (Test-Path $StaleWebView2Bootstrapper) {
    Remove-Item -Path $StaleWebView2Bootstrapper -Force
}

Write-Output 'Building Mnemos installer'
# The installer's publisher URL is the public source repository (script/source-url).
$SOURCE_URL = $env:MNEMOS_SOURCE_URL
if (-not $SOURCE_URL) {
    $SOURCE_URL = Get-Content -Path (Join-Path $WORKSPACE_ROOT_DIR 'script\source-url') |
        Where-Object { $_.Trim() -and -not $_.TrimStart().StartsWith('#') } |
        Select-Object -First 1
}
$SOURCE_URL = "$SOURCE_URL".Trim().TrimEnd('/')
$ISCC_ARGS = @(
    "$WINDOWS_INSTALLER_DIR\windows-installer.iss",
    "/DReleaseChannel=$CHANNEL",
    "/DMyAppExeName=$BINARY_NAME",
    "/DTargetProfileDir=$CARGO_TARGET_OUTPUT_DIR",
    "/DMyAppName=$APP_NAME",
    "/DMyAppVersion=$env:GIT_RELEASE_TAG",
    "/DArch=$ARCH",
    "/DOutputName=$INSTALLER_NAME"
)
if ($SOURCE_URL) {
    $ISCC_ARGS += "/DMyAppURL=$SOURCE_URL"
}
# Also accept the sign tool command via env var
if (-not $SIGN_TOOL_CMD -and $env:SIGN_TOOL_CMD) {
    $SIGN_TOOL_CMD = $env:SIGN_TOOL_CMD
}
if ($SIGN_TOOL_CMD) {
    $ISCC_ARGS += '/DSIGN_TOOL=1'
    $ISCC_ARGS += "/Scodesign=$SIGN_TOOL_CMD"
}
# Resolve ISCC.exe. Inno Setup may be installed per-user (not on PATH).
$ISCC_CMD = $null
$ISCC_ON_PATH = Get-Command ISCC -ErrorAction SilentlyContinue
if ($ISCC_ON_PATH) { $ISCC_CMD = $ISCC_ON_PATH.Source }
if (-not $ISCC_CMD) {
    foreach ($candidate in @(
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
    )) {
        if (Test-Path $candidate) { $ISCC_CMD = $candidate; break }
    }
}
if (-not $ISCC_CMD) {
    Write-Output "ISCC.exe not found on PATH or in standard install locations. Install Inno Setup 6 from https://jrsoftware.org/isdl.php"
    exit 1
}
$global:LASTEXITCODE = $null
$ErrorActionPreference = 'Continue'
& $ISCC_CMD @ISCC_ARGS
$code = $LASTEXITCODE
$ErrorActionPreference = 'Stop'
if ($code -ne 0) {
    Write-Output "Failed to build $APP_NAME installer (exit code $code)"
    exit 1
}

# If this is being run within a GitHub action, set an output variable with the
# location of the installer so it can be referenced by subsequent actions.
if ($env:GITHUB_ACTIONS -eq 'true') {
    Write-Output '::echo::on'
    $INSTALLER_PATH = $INSTALLER_PATH -replace '\\', '/'
    "installer_path=$INSTALLER_PATH" >> "$env:GITHUB_OUTPUT"
    "pdb_file_path=$PDB_PATH" >> "$env:GITHUB_OUTPUT"
    Write-Output '::echo::off'
}
