#!/usr/bin/env powershell
#
# Cut a Mnemos stable release from this machine.
#
#   1. Stamp a version string in the format the autoupdate client can order
#      (v0.<yyyy.MM.dd.HH.mm>.stable_<patch>, in UTC).
#   2. Build the installer via bundle.ps1, as an official release (MNEMOS_OFFICIAL_RELEASE=1).
#   3. Upload the installer to the R2 release bucket.
#   4. Publish channel_versions.json so running clients see the new version: sign the manifest's
#      per-platform payload (sha256 and size of each asset) and a SHA256SUMS file, over only the
#      assets a build vouched for. The installer of this run is one. The other platform's asset is
#      one when -ExpectAsset passes its sha256 (the Mac run prints the exact value, and
#      .github/workflows/release.yml passes it between jobs); for -Rollback, the version's own
#      signed SHA256SUMS vouches. Each is downloaded from the public URL, exactly as a client
#      would, and must match before anything is signed. A served asset nobody vouched for is
#      never signed.
#
# The manifest names one version for every platform. Builds from before signing read only its
# unsigned stable.version, and a Mac runs Mnemos too, so step 4 only happens once Mnemos-arm64.dmg
# for the same version is in the bucket (checked before building and again before publishing). A
# release is two steps; see RELEASING.md:
#
#   Windows first:  ./script/windows/release.ps1 -SkipManifest        # uploads .exe only
#                   ./script/macos/release --version <that version> \
#                       --expect-asset MnemosSetup.exe=<sha256>:<size>  # uploads .dmg, publishes
#
#   Mac first:      ./script/macos/release --skip-manifest            # uploads .dmg only
#                   ./script/windows/release.ps1 -Version <that one> `
#                       -ExpectAsset Mnemos-arm64.dmg=<sha256>:<size>   # builds .exe, publishes
#
# The first run prints the exact --expect-asset / -ExpectAsset value for its asset.
#
# A bare ./script/windows/release.ps1 (fresh version, manifest on) therefore always refuses before
# it builds, unless -AllowMissingMac says a Mac-less release is deliberate.
#
# Every run except -DryRun ends with "RELEASE OK <version>" or "RELEASE FAILED: <reason>" and a
# matching exit code.
#
# Requires (see .twincurrent-build/.env or the environment):
#   CLOUDFLARE_ACCOUNT_ID   Cloudflare account the bucket lives in.
#   CLOUDFLARE_API_TOKEN    R2 API token with "Object Read & Write" on the release bucket.
#   MNEMOS_RELEASES_BUCKET  R2 bucket name. Defaults to "mnemos-releases".
#   MNEMOS_UPDATE_SIGNING_KEY
#                           The update signing key (mnemos-update-sec:...). Needed to publish the
#                           manifest; its public key must be in script/update-signing.pub.
# Optional:
#   MNEMOS_PUBLIC_RELEASES_URL  Where clients download from. Default
#                               https://admin.twincurrent.dk/releases.
#   MNEMOS_MANIFEST_URL         The manifest clients read. Default
#                               $MNEMOS_PUBLIC_RELEASES_URL/channel_versions.json.
#   MNEMOS_UPDATE_PUBLIC_KEYS   Comma-separated public keys, instead of script/update-signing.pub.
#   MNEMOS_UPDATE_BY            Optional update hints, signed into the manifest: an RFC 3339 time
#   MNEMOS_SOFT_CUTOFF          by which clients should update, the oldest supported version, and
#   MNEMOS_LAST_PROMINENT_UPDATE  the latest version with a prominent update. Unset, none is sent.
#   MNEMOS_GITHUB_RELEASE_REPO  owner/repo: also publish the assets, SHA256SUMS and its signature
#                               as a GitHub release (needs gh signed in). Unset, no GitHub release.
#   MNEMOS_RELEASE_COMMIT       The commit a -PublishOnly run's GitHub release points at.
#
# Usage:
#   ./script/windows/release.ps1 -SkipManifest   # build + upload, publish later
#   ./script/windows/release.ps1 -Version <v>    # build + publish once <v>'s .dmg is up
#   ./script/windows/release.ps1 -PublishOnly -Version <v> -ExpectAsset <dmg>=<sha>,<exe>=<sha>
#                                                # publish an uploaded pair
#   ./script/windows/release.ps1 -DryRun         # build + show what would be published
#   ./script/windows/release.ps1 -SkipBuild      # upload the installer already in Output/ under
#                                                # the version it was built as (its ProductVersion)
#   ./script/windows/release.ps1 -Rollback v0.2026.07.30.11.02.stable_01

Param (
    # Explicit version string. Generated from the current time when omitted.
    [String]$Version = '',

    # Patch counter, bumped by hand only when cutting two releases in the same minute.
    [ValidatePattern('^\d{2}$')]
    [String]$Patch = '01',

    [ValidateSet('x64', 'arm64')]
    [String]$ARCH = 'x64',

    # Reuse the installer already sitting in script/windows/Output. Its version is read from the
    # installer (ProductVersion); with -Version the two must match.
    [Switch]$SkipBuild,

    # Build and stage everything, print the manifest, upload nothing.
    [Switch]$DryRun,

    # Point the manifest at an already-uploaded version instead of building. The named version's
    # installer must still exist in the bucket.
    [String]$Rollback = '',

    # Upload the installer but leave the manifest alone. Use when the same version also needs a
    # macOS build uploaded before any client is told the version exists.
    [Switch]$SkipManifest,

    # Publish the manifest for an existing version without building or uploading anything.
    [Switch]$PublishOnly,

    # '<name>=<sha256>[:<size>]' for each asset another run built, as that run printed it.
    # Publishing refuses unless the served bytes match, and signs no asset without a digest from a
    # build (this run's own installer counts as one).
    [String[]]$ExpectAsset = @(),

    # Publish the manifest even though Mnemos-arm64.dmg for this version is not in the bucket.
    # Every Mac install then fails its update until a .dmg is uploaded. Without it the release
    # refuses, before building, to publish a version the Macs cannot download.
    [Switch]$AllowMissingMac,

    # Build (or, with -SkipBuild, upload) a build made from a tree with uncommitted or untracked
    # changes. Without it the release refuses: the About page and the bundled NOTICE name the
    # commit as this build's source (AGPL section 6), so the binary has to be exactly that commit.
    # Never publish a build made with it.
    [Switch]$AllowDirty,

    # Release a commit the public source repository (script/source-url) does not serve
    # anonymously. Without it the release refuses, because the source offer in the About page and
    # the NOTICE would point at nothing. Only for builds that never leave TwinCurrent; the bundled
    # NOTICE says so.
    [Switch]$AllowUnpublished,

    # How many releases to keep in the bucket besides the one just published and the live one.
    # Older ones are deleted after a successful publish; nothing is pruned under -SkipManifest.
    [ValidateRange(1, 100)]
    [Int]$KeepReleases = 5
)

$ErrorActionPreference = 'Stop'

# Every run except -DryRun ends in one of two lines, "RELEASE OK <version>" or "RELEASE FAILED:
# <reason>", with a matching exit code. Run detached with its output in a file, an unhandled error
# would otherwise leave only a log that stops mid-way.
trap {
    Write-Output "RELEASE FAILED: $($_.Exception.Message)"
    exit 1
}

$WORKSPACE_ROOT_DIR = (Resolve-Path "$PSScriptRoot\..\..").Path
$OUTPUT_DIR = "$PSScriptRoot\Output"

# ---------------------------------------------------------------------------
# Corresponding Source (AGPL section 6)
# ---------------------------------------------------------------------------

# Mnemos IDE: the source offer in the About page and the bundled NOTICE names the commit a build
# comes from and links to it on the public repository, so a release must be exactly a commit that
# repository has. Every upload is checked, tagged (local annotated tag release/<version>-windows,
# never pushed) and gets Mnemos-source-<commit>.tar.gz (git archive plus `cargo vendor`) next to
# it. Keep these checks in step with script/macos/release.
$SourceUrl = $env:MNEMOS_SOURCE_URL
if (-not $SourceUrl) {
    $SourceUrl = Get-Content -Path (Join-Path $WORKSPACE_ROOT_DIR 'script\source-url') |
        Where-Object { $_.Trim() -and -not $_.TrimStart().StartsWith('#') } |
        Select-Object -First 1
}
$SourceUrl = "$SourceUrl".Trim().TrimEnd('/')

# Runs git in the workspace and returns its stdout lines; throws on a non-zero exit. Native
# commands run under 'Continue' (see script/windows/bundle.ps1 for why).
function Invoke-Git {
    $global:LASTEXITCODE = 0
    $ErrorActionPreference = 'Continue'
    $out = & git -C $WORKSPACE_ROOT_DIR @args 2>$null
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($code -ne 0) { throw "git $($args -join ' ') failed (exit $code)" }
    return $out
}

# Refuses a tree with uncommitted changes or untracked, non-ignored files. Untracked files count:
# they are not in the published source of the commit the NOTICE names.
function Assert-CleanTree {
    if ($AllowDirty) { return }
    $dirty = Invoke-Git status --porcelain
    if ($dirty) {
        throw "Refusing to release from a tree with uncommitted or untracked changes:`n$($dirty -join "`n")`nCommit, remove or gitignore them first (or pass -AllowDirty for a build you will not publish)."
    }
}

# Refuses a commit the public repository does not have. Fetches its branches and tags into
# refs/mnemos-public/ (no remote is added) and asks whether any of them contains the commit. The
# fetch is anonymous (no credential helper, no prompt), so a private repository fails it even when
# this machine's git could sign in to it: the source offer has to work for everyone.
$script:PublishedChecked = ''
function Assert-Published {
    Param([String]$Commit)
    if ($AllowUnpublished -or ($script:PublishedChecked -eq $Commit)) { return }
    if (-not $SourceUrl) { throw "No source URL (script\source-url or MNEMOS_SOURCE_URL); cannot check that $Commit is published." }
    Write-Output "Checking that $Commit is on $SourceUrl ..."
    $savedPrompt = $env:GIT_TERMINAL_PROMPT
    $savedAskPass = $env:GIT_ASKPASS
    $env:GIT_TERMINAL_PROMPT = '0'
    $env:GIT_ASKPASS = 'echo'
    try {
        Invoke-Git -c credential.helper= fetch --quiet --no-tags --no-write-fetch-head $SourceUrl `
            '+refs/heads/*:refs/mnemos-public/heads/*' '+refs/tags/*:refs/mnemos-public/tags/*' | Out-Null
    } catch {
        throw "Cannot fetch $SourceUrl without signing in. The public source repository must exist, be public and be reachable before a release (pass -AllowUnpublished for a build that never leaves TwinCurrent)."
    } finally {
        $env:GIT_TERMINAL_PROMPT = $savedPrompt
        $env:GIT_ASKPASS = $savedAskPass
    }
    if (-not (Invoke-Git for-each-ref --contains $Commit refs/mnemos-public)) {
        throw "Commit $Commit is not on $SourceUrl. Push it there first: the About page and the bundled NOTICE link to it as this build's source (or pass -AllowUnpublished for a build that never leaves TwinCurrent)."
    }
    $script:PublishedChecked = $Commit
}

# Creates the local annotated tag release/<version>-windows on the release commit, or keeps it when
# it already names that commit. Never pushes.
function New-ReleaseTag {
    Param([String]$Commit)
    $tag = "release/$Version-windows"
    $existing = ''
    try { $existing = "$(Invoke-Git rev-parse -q --verify "refs/tags/$tag^{commit}")".Trim() } catch { $existing = '' }
    if ($existing) {
        if ($existing -ne $Commit) { throw "Tag $tag already names $existing, not $Commit; refusing to move it." }
        Write-Output "Tag $tag already names $Commit."
        return
    }
    Invoke-Git tag -a -m "Mnemos $Version (Windows $ARCH), built from $Commit" $tag $Commit | Out-Null
    Write-Output "Tagged $Commit as $tag (local only; push it to the public repository)."
}

# Writes the source archive for a commit: its tree (git archive) plus every Rust dependency
# vendored by `cargo vendor`, with vendor-config.toml for `cargo build --config vendor-config.toml`.
# The archive goes through a file: Windows PowerShell 5.1 corrupts binary data piped between
# native commands.
function New-SourceArchive {
    Param([String]$Commit, [String]$OutFile)
    $prefix = "mnemos-$($Commit.Substring(0, 12))"
    $work = Join-Path ([IO.Path]::GetTempPath()) ("mnemos-source-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $work | Out-Null
    Write-Output "Building source archive $OutFile ..."
    try {
        $tarFile = Join-Path $work 'tree.tar'
        Invoke-Git archive --format=tar "--prefix=$prefix/" -o $tarFile $Commit | Out-Null
        $global:LASTEXITCODE = 0
        $ErrorActionPreference = 'Continue'
        & tar.exe -xf $tarFile -C $work
        $code = $LASTEXITCODE
        $ErrorActionPreference = 'Stop'
        if ($code -ne 0) { throw "tar -x failed (exit $code)" }
        Remove-Item -Path $tarFile -Force
        Push-Location (Join-Path $work $prefix)
        try {
            $global:LASTEXITCODE = 0
            $ErrorActionPreference = 'Continue'
            $config = & cargo vendor --locked --versioned-dirs vendor 2>$null
            $code = $LASTEXITCODE
            $ErrorActionPreference = 'Stop'
            if ($code -ne 0) { throw "cargo vendor failed (exit $code); no source archive for $Commit." }
            Set-Content -Path 'vendor-config.toml' -Value $config -Encoding ascii
        } finally {
            Pop-Location
        }
        $global:LASTEXITCODE = 0
        $ErrorActionPreference = 'Continue'
        & tar.exe -czf $OutFile -C $work $prefix
        $code = $LASTEXITCODE
        $ErrorActionPreference = 'Stop'
        if ($code -ne 0) { throw "tar -cz failed (exit $code)" }
    } finally {
        Remove-Item -Path $work -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# The commit and dirty marker prepare_bundled_resources.ps1 wrote into the staged NOTICE that
# bundle.ps1 packed into the installer. Refuses a NOTICE newer than the installer: it then
# belongs to a later bundle run, not to the installer being published.
function Get-BuiltSource {
    Param([String]$Installer)
    $triple = if ($ARCH -eq 'arm64') { 'aarch64-pc-windows-msvc' } else { 'x86_64-pc-windows-msvc' }
    $notice = Join-Path $WORKSPACE_ROOT_DIR "target\$triple\rlto\resources\NOTICE"
    if (-not (Test-Path $notice)) { throw "No staged NOTICE at $notice; cannot tell which commit $Installer was built from. Drop -SkipBuild to rebuild it." }
    if ((Get-Item $notice).LastWriteTime -gt (Get-Item $Installer).LastWriteTime) {
        throw "$notice is newer than $Installer, so it may describe a different build. Drop -SkipBuild to rebuild it."
    }
    $lines = Get-Content -Path $notice
    $commit = ''
    foreach ($line in $lines) {
        if ($line -match '^This copy of Mnemos IDE was built from commit ([0-9a-f]{40})\.$') { $commit = $Matches[1]; break }
    }
    return @{
        Commit = $commit
        Dirty  = [bool]($lines | Where-Object { $_ -like 'This is NOT a release build*' })
    }
}

if (-not ($SkipBuild -or $PublishOnly -or $Rollback)) {
    Assert-CleanTree
    Assert-Published -Commit ("$(Invoke-Git rev-parse HEAD)".Trim())
}

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

# Load .twincurrent-build/.env if present so credentials don't have to be exported by hand.
#
# The two secrets never stay in the environment: they are held in script variables and removed
# from this process's environment, so no child inherits them. Every build script and proc macro
# of the app (bundle.ps1, `cargo vendor`, update_signing's own compile) therefore runs without
# them. Only the commands that need one get it: the R2 token goes to the curl calls
# (Get-R2Credentials), the signing key to the prebuilt update_signing binary for the length of
# one call (Invoke-UpdateSigningWithKey).
$script:R2Token = "$env:CLOUDFLARE_API_TOKEN"
$script:SigningKey = "$env:MNEMOS_UPDATE_SIGNING_KEY"
Remove-Item -Path env:CLOUDFLARE_API_TOKEN, env:MNEMOS_UPDATE_SIGNING_KEY -ErrorAction SilentlyContinue
$envFile = Join-Path $WORKSPACE_ROOT_DIR '.twincurrent-build\.env'
if (Test-Path $envFile) {
    foreach ($line in Get-Content $envFile) {
        if ($line -match '^\s*([A-Z0-9_]+)\s*=\s*(.*?)\s*$') {
            $name = $Matches[1]
            $value = $Matches[2].Trim('"').Trim("'")
            # Real environment variables win over the file.
            if ($name -eq 'CLOUDFLARE_API_TOKEN') {
                if (-not $script:R2Token) { $script:R2Token = $value }
            } elseif ($name -eq 'MNEMOS_UPDATE_SIGNING_KEY') {
                if (-not $script:SigningKey) { $script:SigningKey = $value }
            } elseif (-not [Environment]::GetEnvironmentVariable($name)) {
                Set-Item -Path "env:$name" -Value $value
            }
        }
    }
}

$BUCKET = if ($env:MNEMOS_RELEASES_BUCKET) { $env:MNEMOS_RELEASES_BUCKET } else { 'mnemos-releases' }
$PublicReleasesUrl = if ($env:MNEMOS_PUBLIC_RELEASES_URL) { $env:MNEMOS_PUBLIC_RELEASES_URL } else { 'https://admin.twincurrent.dk/releases' }
$PublicReleasesUrl = $PublicReleasesUrl.Trim().TrimEnd('/')
$ManifestUrl = if ($env:MNEMOS_MANIFEST_URL) { $env:MNEMOS_MANIFEST_URL.Trim() } else { "$PublicReleasesUrl/channel_versions.json" }

function Assert-Credentials {
    $missing = @()
    if (-not $env:CLOUDFLARE_ACCOUNT_ID) { $missing += 'CLOUDFLARE_ACCOUNT_ID' }
    if (-not $script:R2Token) { $missing += 'CLOUDFLARE_API_TOKEN' }
    if ($missing.Count -gt 0) {
        throw @"
Missing R2 credentials: $($missing -join ', ')

Set them in $envFile or in the environment. The API token needs the
"Object Read & Write" permission on the mnemos-releases bucket in account $($env:CLOUDFLARE_ACCOUNT_ID).
"@
    }
}

# Derive S3 credentials for the R2 API from the Cloudflare API token.
#
# Uploads go over R2's S3 endpoint rather than through wrangler. wrangler's `r2 object put` calls
# the Cloudflare REST API, which demands account-wide "Workers R2 Storage: Edit" - it returns 403
# for a token scoped to a single bucket. The S3 endpoint accepts the scoped token, so the release
# credential stays limited to this one bucket instead of being able to delete every bucket we own.
#
# Access Key ID is the token's own id; Secret Access Key is the SHA-256 of the token value.
function Get-R2Credentials {
    $verifyUrl = "https://api.cloudflare.com/client/v4/accounts/$($env:CLOUDFLARE_ACCOUNT_ID)/tokens/verify"
    # The header goes through stdin (-H @-), so the token is not on curl's command line either.
    $response = "Authorization: Bearer $($script:R2Token)" | curl.exe -s -H '@-' $verifyUrl
    $parsed = $response | ConvertFrom-Json
    if (-not $parsed.success) {
        throw "Could not verify CLOUDFLARE_API_TOKEN against the Cloudflare API. Is it still valid?"
    }

    $sha = [System.Security.Cryptography.SHA256]::Create()
    $secret = -join ($sha.ComputeHash(
            [Text.Encoding]::UTF8.GetBytes($script:R2Token)
        ) | ForEach-Object { $_.ToString('x2') })

    return @{
        AccessKeyId = $parsed.result.id
        SecretKey   = $secret
        Endpoint    = "https://$($env:CLOUDFLARE_ACCOUNT_ID).r2.cloudflarestorage.com"
    }
}

# HTTP status of a published asset, as a client downloading it would see it. -L is essential: the
# admin route answers 302 for every path before anything asks the bucket, so without it a missing
# asset looks present. '000' when the request fails outright.
function Get-AssetCode {
    Param([String]$V, [String]$Name)
    $out = curl.exe -sIL -o NUL -w '%{http_code}' --max-time 30 "$PublicReleasesUrl/stable/$V/$Name"
    $last = @($out) | Select-Object -Last 1
    if ($null -eq $last) { return '000' }
    return "$last".Trim()
}

# ---------------------------------------------------------------------------
# Update signing
# ---------------------------------------------------------------------------

# The release-side signing tool (crates/channel_versions/src/bin/update_signing.rs), shared with
# script/macos/release so both sign and stamp the same way. A small crate; dev profile. Returns its
# stdout lines; throws on failure.
#
# It is compiled once, by a cargo that never sees a secret (see Config), and then run as a plain
# binary. Only Invoke-UpdateSigningWithKey hands it the signing key, for the length of that one
# call, so no build script ever runs next to the key.
$script:UpdateSigningBin = ''
function Build-UpdateSigning {
    if ($script:UpdateSigningBin) { return }
    $global:LASTEXITCODE = 0
    $ErrorActionPreference = 'Continue'
    Push-Location $WORKSPACE_ROOT_DIR
    try {
        & cargo build --quiet --locked -p channel_versions --bin update_signing
        $code = $LASTEXITCODE
        $metadata = & cargo metadata --format-version 1 --no-deps --locked
    } finally {
        Pop-Location
        $ErrorActionPreference = 'Stop'
    }
    if ($code -ne 0) { throw "Building update_signing failed (exit $code)" }
    $targetDir = ($metadata | Out-String | ConvertFrom-Json).target_directory
    $bin = Join-Path $targetDir 'debug\update_signing.exe'
    if (-not (Test-Path $bin)) { throw "update_signing was built but is not at $bin." }
    $script:UpdateSigningBin = $bin
}
function Invoke-UpdateSigning {
    Build-UpdateSigning
    $global:LASTEXITCODE = 0
    $ErrorActionPreference = 'Continue'
    try {
        $out = & $script:UpdateSigningBin @args
        $code = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = 'Stop'
    }
    if ($code -ne 0) { throw "update_signing $($args[0]) failed (exit $code)" }
    return $out
}
function Invoke-UpdateSigningWithKey {
    Build-UpdateSigning
    $env:MNEMOS_UPDATE_SIGNING_KEY = $script:SigningKey
    try {
        return (Invoke-UpdateSigning @args)
    } finally {
        Remove-Item -Path env:MNEMOS_UPDATE_SIGNING_KEY -ErrorAction SilentlyContinue
    }
}

# The public keys a build from this tree bakes in; app/build.rs reads the same two sources.
$PublicKeysFile = Join-Path $WORKSPACE_ROOT_DIR 'script\update-signing.pub'
if ($env:MNEMOS_UPDATE_PUBLIC_KEYS) {
    $PublicKeysFile = Join-Path ([IO.Path]::GetTempPath()) ("mnemos-update-keys-" + [Guid]::NewGuid().ToString('N') + '.pub')
    Set-Content -Path $PublicKeysFile -Value ($env:MNEMOS_UPDATE_PUBLIC_KEYS -split ',') -Encoding ascii
}

# An official build does not compile without a public key (app/build.rs); say so before building.
function Assert-PublicKey {
    $keys = Get-Content -Path $PublicKeysFile | Where-Object { $_.Trim() -and -not $_.TrimStart().StartsWith('#') -and $_.Contains('mnemos-update-pub:') }
    if (-not $keys) {
        throw "No update signing public key in $PublicKeysFile. An official build refuses to compile without one; see RELEASING.md, `"Update signing`"."
    }
}

# Publishing signs the manifest, so the key must be here and must be one the builds trust.
function Assert-SigningKey {
    if (-not $script:SigningKey) {
        throw 'MNEMOS_UPDATE_SIGNING_KEY is not set; publishing the manifest needs it (or pass -SkipManifest).'
    }
    Invoke-UpdateSigningWithKey check-key --public-keys $PublicKeysFile | Write-Output
}

# The platforms clients ask about (channel_versions::signed::current_platform) and the asset each
# downloads: installer_file_name() in app/src/autoupdate/windows.rs, dmg_name() in mac.rs. Keep in
# step with script/macos/release. A platform whose asset is not published gets no entry in the
# signed payload, so its clients see no update instead of failing on a 404.
$PlatformAssets = [ordered]@{
    'macos-aarch64'   = 'Mnemos-arm64.dmg'
    'windows-x86_64'  = 'MnemosSetup.exe'
    'windows-aarch64' = 'MnemosSetup-arm64.exe'
}

# Digests a build vouched for, by asset name: @{ Sha = <lowercase hex>; Size = <bytes or ''> }.
# Filled from -ExpectAsset, from this run's own installer once it is hashed, and for -Rollback from
# the version's signed SHA256SUMS (Add-SignedSumsDigests).
$script:ExpectedAssets = @{}
function Add-ExpectedAsset {
    Param([String]$Spec, [String]$From)
    if ($Spec -notmatch '^([A-Za-z0-9._-]+)=([0-9a-fA-F]{64})(?::([0-9]+))?$') {
        throw "$From $Spec is not <name>=<sha256>[:<size>]."
    }
    $name = $Matches[1]
    $entry = @{ Sha = $Matches[2].ToLower(); Size = "$($Matches[3])" }
    $previous = $script:ExpectedAssets[$name]
    if ($previous) {
        if ($previous.Sha -ne $entry.Sha -or ($previous.Size -and $entry.Size -and $previous.Size -ne $entry.Size)) {
            throw "Two different digests for $name ($($previous.Sha) and $($entry.Sha)); refusing to guess."
        }
        if (-not $previous.Size) { $previous.Size = $entry.Size }
        return
    }
    $script:ExpectedAssets[$name] = $entry
}
foreach ($spec in @($ExpectAsset | ForEach-Object { "$_" -split ',' } | Where-Object { $_.Trim() })) {
    Add-ExpectedAsset -Spec $spec.Trim() -From '-ExpectAsset'
}

# -Rollback re-publishes a version whose release was signed before: its SHA256SUMS, checked
# against its signature and the public keys, says which bytes were vouched for then.
function Add-SignedSumsDigests {
    $dir = Join-Path ([IO.Path]::GetTempPath()) ("mnemos-sums-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $dir | Out-Null
    try {
        foreach ($name in @('SHA256SUMS', 'SHA256SUMS.sig')) {
            $global:LASTEXITCODE = 0
            & curl.exe -sfL --max-time 60 -o (Join-Path $dir $name) "$PublicReleasesUrl/stable/$Version/$name"
            if ($LASTEXITCODE -ne 0) { throw "Cannot download $name for $Version (curl exit $LASTEXITCODE); without it nothing vouches for its assets. Pass -ExpectAsset." }
        }
        Invoke-UpdateSigning verify-sums --in (Join-Path $dir 'SHA256SUMS') --sig (Join-Path $dir 'SHA256SUMS.sig') --public-keys $PublicKeysFile | Out-Host
        foreach ($line in Get-Content -Path (Join-Path $dir 'SHA256SUMS')) {
            if ($line -match '^([0-9a-f]{64})  (\S+)$' -and $PlatformAssets.Values -contains $Matches[2]) {
                Add-ExpectedAsset -Spec "$($Matches[2])=$($Matches[1])" -From "$Version's SHA256SUMS:"
            }
        }
    } finally {
        Remove-Item -Path $dir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# Signs the release over the assets a build vouched for ($script:ExpectedAssets). Each is
# downloaded from the public URL into a fresh temp dir, exactly as a client would, and must have
# the expected sha256 (and size, when known); then the signed manifest, SHA256SUMS and
# SHA256SUMS.sig are written and checked over those bytes. An asset that is served but that no
# build vouched for is left out, so its platform sees no update rather than unvouched bytes.
# Returns the temp dir. Uploads nothing.
function New-SignedRelease {
    Param([String]$ManifestPath)
    $dir = Join-Path ([IO.Path]::GetTempPath()) ("mnemos-signed-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $dir | Out-Null
    $sums = Join-Path $dir 'SHA256SUMS'
    $sumLines = @()
    $assetArgs = @()
    foreach ($platform in $PlatformAssets.Keys) {
        $name = $PlatformAssets[$platform]
        $code = Get-AssetCode -V $Version -Name $name
        $expected = $script:ExpectedAssets[$name]
        if (-not $expected) {
            if ($code -eq '200') {
                Write-Warning "$name is served for $Version, but no build in this run vouched for its bytes (-ExpectAsset); $platform gets no release in this manifest."
            } else {
                Write-Host "No $name for $Version (HTTP $code); $platform gets no release in this manifest."
            }
            continue
        }
        if ($code -ne '200') { throw "$name was built for $Version but is not downloadable (HTTP $code); manifest NOT published." }
        $file = Join-Path $dir $name
        Write-Host "Downloading $name to check it against its build ..."
        $global:LASTEXITCODE = 0
        & curl.exe -sfL --max-time 1800 -o $file "$PublicReleasesUrl/stable/$Version/$name"
        if ($LASTEXITCODE -ne 0) { throw "Cannot download $PublicReleasesUrl/stable/$Version/$name (curl exit $LASTEXITCODE); manifest NOT published." }
        $digest = "$(Invoke-UpdateSigning hash $file)".Trim() -split ' '
        $sha = $digest[0]
        $size = $digest[1]
        if ($sha -ne $expected.Sha) {
            throw "$name as served ($sha) is not the bytes its build produced ($($expected.Sha)); manifest NOT published."
        }
        if ($expected.Size -and ($size -ne $expected.Size)) {
            throw "$name as served is $size bytes, its build produced $($expected.Size); manifest NOT published."
        }
        $assetArgs += @('--asset', "$platform=${name}:${sha}:$size")
        $sumLines += "$sha  $name"
    }
    if ($assetArgs.Count -eq 0) { throw "No asset of $Version is both downloadable and vouched for; manifest NOT published." }
    if ($SourcePath -and (Test-Path $SourcePath)) {
        $sourceSha = (Get-FileHash -Path $SourcePath -Algorithm SHA256).Hash.ToLower()
        $sumLines += "$sourceSha  $SourceName"
    }
    # LF line endings and no BOM, so the file hashes and verifies the same everywhere.
    [IO.File]::WriteAllText($sums, (($sumLines -join "`n") + "`n"), (New-Object Text.UTF8Encoding($false)))
    # Optional update hints, signed into the payload (see `update_signing manifest --help`).
    $hintArgs = @()
    if ($env:MNEMOS_UPDATE_BY) { $hintArgs += @('--update-by', $env:MNEMOS_UPDATE_BY.Trim()) }
    if ($env:MNEMOS_SOFT_CUTOFF) { $hintArgs += @('--soft-cutoff', $env:MNEMOS_SOFT_CUTOFF.Trim()) }
    if ($env:MNEMOS_LAST_PROMINENT_UPDATE) { $hintArgs += @('--last-prominent-update', $env:MNEMOS_LAST_PROMINENT_UPDATE.Trim()) }
    $manifestArgs = @('manifest', '--version', $Version) + $assetArgs + $hintArgs + @('--out', $ManifestPath)
    Invoke-UpdateSigningWithKey @manifestArgs | Out-Host
    Invoke-UpdateSigning verify-manifest --manifest $ManifestPath --public-keys $PublicKeysFile --expect-version $Version | Out-Host
    Invoke-UpdateSigningWithKey sign-sums --in $sums --out (Join-Path $dir 'SHA256SUMS.sig') | Out-Host
    Invoke-UpdateSigning verify-sums --in $sums --sig (Join-Path $dir 'SHA256SUMS.sig') --public-keys $PublicKeysFile | Out-Host
    return $dir
}

# Optional GitHub release with the downloaded assets, SHA256SUMS and its signature. A failure here
# only warns: the manifest is already live.
function Publish-GitHubRelease {
    Param([String]$Dir, [String]$Commit)
    $repo = $env:MNEMOS_GITHUB_RELEASE_REPO
    if (-not $repo) {
        Write-Output 'MNEMOS_GITHUB_RELEASE_REPO is not set; no GitHub release.'
        return
    }
    $tag = "release/$Version"
    $files = @(Get-ChildItem -Path $Dir -File | ForEach-Object { $_.FullName })
    $global:LASTEXITCODE = 0
    $ErrorActionPreference = 'Continue'
    & gh release view $tag -R $repo *> $null
    $exists = ($LASTEXITCODE -eq 0)
    if ($exists) {
        & gh release upload $tag -R $repo --clobber @files
    } elseif ($Commit) {
        & gh release create $tag -R $repo --target $Commit --title "Mnemos $Version" `
            --notes "Mnemos $Version, built from $Commit. Check downloads against SHA256SUMS; SHA256SUMS.sig is signed with a key listed in script/update-signing.pub (update_signing verify-sums checks it)." `
            @files
    } else {
        Write-Warning "No GitHub release $tag yet and no commit known in this run (-PublishOnly/-Rollback); create it from the run that built an asset."
        $global:LASTEXITCODE = 0
    }
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($code -ne 0) { Write-Warning "Publishing the GitHub release $tag on $repo failed (exit $code)." }
}

# The release version the live manifest names, or ''.
function Get-LiveVersion {
    try {
        $published = Invoke-RestMethod -Uri $ManifestUrl -TimeoutSec 20
        return "$($published.stable.version)".Trim()
    } catch {
        return ''
    }
}

# ---------------------------------------------------------------------------
# Version
# ---------------------------------------------------------------------------

# The autoupdate client orders releases by parsing the embedded timestamp out of this string
# (crates/channel_versions/src/lib.rs). Deviating from the shape breaks ordering, which is what
# stops the client from "updating" backwards onto an older build.
#
# Stamped in UTC. Releases before the switch were stamped in local time, up to two hours ahead, so
# stamp-version refuses a version that would not sort after the live one or any local release tag,
# and says how long to wait.
function New-VersionString {
    Param([String]$PatchNumber)
    $previousArgs = @()
    $live = Get-LiveVersion
    if ($live) { $previousArgs += @('--previous', $live) }
    foreach ($tag in @(Invoke-Git tag -l 'release/v*')) {
        $v = ("$tag".Trim() -replace '^release/', '') -replace '-(macos|windows)$', ''
        if ($v) { $previousArgs += @('--previous', $v) }
    }
    $stampArgs = @('stamp-version', '--patch', $PatchNumber) + $previousArgs
    return "$(Invoke-UpdateSigning @stampArgs)".Trim()
}

$VersionPattern = '^v\d+\.\d{4}\.\d{2}\.\d{2}\.\d{2}\.\d{2}\.stable_\d+$'

if ($ARCH -eq 'arm64') {
    $INSTALLER_NAME = 'MnemosSetup-arm64.exe'
} else {
    $INSTALLER_NAME = 'MnemosSetup.exe'
}
$INSTALLER_PATH = Join-Path $OUTPUT_DIR $INSTALLER_NAME

# The version an installer was built as. Inno Setup stamps AppVersion (bundle.ps1 passes
# GIT_RELEASE_TAG as MyAppVersion) into the file's ProductVersion. Returns '' when the file is
# missing or the field is empty; callers treat it as authoritative only when it is a valid
# release version string.
function Get-InstallerVersion {
    Param([String]$Path)
    if (-not (Test-Path $Path)) { return '' }
    $raw = (Get-Item $Path).VersionInfo.ProductVersion
    if ($null -eq $raw) { return '' }
    return "$raw".Trim()
}

# A new version is stamped only for a fresh build. -SkipBuild uploads bytes that already carry a
# version (the client compiles GIT_RELEASE_TAG in), so that version is the release. Stamping a new
# one would publish the old build under a new name: every client downloads it, still reports the
# old version, and reinstalls it again on every check until the next real release.
if ($Rollback) {
    $Version = $Rollback
    $SkipBuild = $true
} elseif ($PublishOnly) {
    if (-not $Version) { throw '-PublishOnly needs -Version: it publishes an existing upload, it cannot guess which.' }
    $SkipBuild = $true
} elseif ($SkipBuild) {
    if (-not (Test-Path $INSTALLER_PATH)) {
        throw "Installer not found at $INSTALLER_PATH. Drop -SkipBuild to build it."
    }
    $builtVersion = Get-InstallerVersion -Path $INSTALLER_PATH
    $builtIsVersion = $builtVersion -match $VersionPattern
    if (-not $Version) {
        if (-not $builtIsVersion) {
            throw "Cannot read a release version from $INSTALLER_PATH (ProductVersion '$builtVersion'). Pass -Version."
        }
        $Version = $builtVersion
        Write-Output "-SkipBuild: $INSTALLER_PATH was built as $Version; releasing it under that version."
    } elseif ($builtIsVersion -and ($builtVersion -ne $Version)) {
        throw "Installer was built as $builtVersion, refusing to publish it as $Version."
    } elseif (-not $builtIsVersion) {
        Write-Warning "Cannot read a release version from $INSTALLER_PATH (ProductVersion '$builtVersion'); trusting -Version $Version."
    }
} elseif (-not $Version) {
    $Version = New-VersionString -PatchNumber $Patch
}

if ($SkipManifest -and ($Rollback -or $PublishOnly)) {
    throw '-SkipManifest contradicts -Rollback/-PublishOnly, whose only effect is to publish the manifest.'
}

# Both modes act on bytes that are already in the bucket, so there is no local installer to upload.
$UploadInstaller = -not ($Rollback -or $PublishOnly)

if ($Version -notmatch $VersionPattern) {
    throw "Version '$Version' is not in the form v0.<yyyy.MM.dd.HH.mm>.stable_<nn>. The client cannot order it."
}

Write-Output "Releasing Mnemos $Version ($ARCH)"

# The build bakes in the update public keys, and publishing signs with the matching secret key;
# both are checked before a 12-minute build rather than after it.
if (-not $SkipBuild) { Assert-PublicKey }
if (-not $SkipManifest -and -not $DryRun) { Assert-SigningKey }

if ($Rollback) { Add-SignedSumsDigests }

# The manifest carries one version for every platform, so publishing it names a version the Macs
# will also try to download. Check that their asset exists before spending a build on a release
# that would then refuse to publish. -Rollback and -PublishOnly get the same check up front.
if (-not $SkipManifest) {
    $dmgCode = Get-AssetCode -V $Version -Name 'Mnemos-arm64.dmg'
    if (($dmgCode -eq '200') -and -not $script:ExpectedAssets['Mnemos-arm64.dmg']) {
        # Served but not vouched for: New-SignedRelease would leave the Macs out of the manifest.
        $dmgCode = "200, but no -ExpectAsset Mnemos-arm64.dmg=<sha256> from its build"
    }
    if ($dmgCode -ne '200') {
        if ($AllowMissingMac) {
            Write-Warning "Mnemos-arm64.dmg for $Version is not in the bucket (HTTP $dmgCode); publishing anyway (-AllowMissingMac)."
        } elseif ($DryRun) {
            Write-Warning "Mnemos-arm64.dmg for $Version is not in the bucket (HTTP $dmgCode). A real run would refuse here."
        } elseif ($UploadInstaller) {
            throw @"
Mnemos-arm64.dmg for $Version is not in the bucket (HTTP $dmgCode).
Publishing the manifest would point every Mac install at a version it cannot download.
Release this side with -SkipManifest and publish once the .dmg is up, or pass -AllowMissingMac
if no Mac needs this version.
"@
        } else {
            throw "Mnemos-arm64.dmg for $Version is not in the bucket (HTTP $dmgCode). Upload it first, or pass -AllowMissingMac if no Mac needs this version."
        }
    }
}

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

if (-not $SkipBuild) {
    Write-Output 'Building installer...'
    $env:GIT_RELEASE_TAG = $Version
    # Only an official release gets an updater (app/build.rs); a plain bundle.ps1 does not.
    $env:MNEMOS_OFFICIAL_RELEASE = '1'
    if ($AllowUnpublished) {
        # prepare_bundled_resources.ps1 then says in the bundled NOTICE that this is an internal build.
        $env:MNEMOS_SOURCE_UNPUBLISHED = '1'
    }
    Push-Location $WORKSPACE_ROOT_DIR
    try {
        $global:LASTEXITCODE = 0
        & "$PSScriptRoot\bundle.ps1" -CHANNEL stable -ARCH $ARCH
        if ($LASTEXITCODE -ne 0) { throw "bundle.ps1 failed with exit code $LASTEXITCODE" }
    } finally {
        Pop-Location
    }

    # A stale installer left in Output/ by an earlier build must not go out under this version.
    # Only a valid version string that differs is fatal: if a future .iss change stops stamping
    # ProductVersion, warn rather than throw away a 12-minute build.
    $builtVersion = Get-InstallerVersion -Path $INSTALLER_PATH
    if ($builtVersion -match $VersionPattern) {
        if ($builtVersion -ne $Version) {
            throw "Installer was built as $builtVersion, refusing to publish it as $Version."
        }
    } else {
        Write-Warning "Cannot read a release version from $INSTALLER_PATH (ProductVersion '$builtVersion'); cannot confirm it is $Version."
    }
}

if ($UploadInstaller) {
    if (-not (Test-Path $INSTALLER_PATH)) {
        throw "Installer not found at $INSTALLER_PATH. Drop -SkipBuild to build it."
    }
    $installer = Get-Item $INSTALLER_PATH
    $sizeMb = [Math]::Round($installer.Length / 1MB, 1)
    $sha = (Get-FileHash -Path $INSTALLER_PATH -Algorithm SHA256).Hash.ToLower()
    Write-Output "Installer: $INSTALLER_PATH ($sizeMb MB, sha256 $sha)"
    # The installer this run holds is the one asset it can vouch for itself.
    $InstallerExpect = "$INSTALLER_NAME=${sha}:$($installer.Length)"
    Add-ExpectedAsset -Spec $InstallerExpect -From 'This run''s installer'

    # The source checks again, on what the installer's NOTICE says it was built from: with
    # -SkipBuild this is the only check, and for a fresh build it confirms the bundle recorded the
    # commit that was checked. -PublishOnly and -Rollback upload nothing; the run that uploaded
    # their installer checked it.
    $built = Get-BuiltSource -Installer $INSTALLER_PATH
    if (-not $built.Commit) { throw "The NOTICE packed into $INSTALLER_PATH names no source commit; refusing to publish it." }
    if ($built.Dirty -and -not $AllowDirty) {
        throw "$INSTALLER_PATH was built from a tree with uncommitted changes (its NOTICE says so); refusing to publish it."
    }
    if (-not $SkipBuild) {
        $head = "$(Invoke-Git rev-parse HEAD)".Trim()
        if ($built.Commit -ne $head) { throw "$INSTALLER_PATH says it was built from $($built.Commit), but HEAD is $head; refusing to publish it." }
    }
    Assert-Published -Commit $built.Commit
    $SourceName = "Mnemos-source-$($built.Commit).tar.gz"
    $SourcePath = Join-Path $OUTPUT_DIR $SourceName
    Write-Output "Source: commit $($built.Commit) ($SourceUrl/tree/$($built.Commit))"
}

# ---------------------------------------------------------------------------
# Manifest
# ---------------------------------------------------------------------------

# Written at publish time by New-SignedRelease: the legacy dev/preview/stable fields
# (ChannelVersions requires all three) plus the signed per-platform payload.
$manifestPath = Join-Path $OUTPUT_DIR 'channel_versions.json'
New-Item -ItemType Directory -Force -Path $OUTPUT_DIR | Out-Null

$installerKey = "stable/$Version/$INSTALLER_NAME"

Write-Output ''
Write-Output "Would upload:"
if ($UploadInstaller) {
    Write-Output "  $BUCKET/$installerKey"
    Write-Output "  $BUCKET/stable/$Version/$SourceName (unless already there)"
    Write-Output "Would tag $($built.Commit) as release/$Version-windows (local only)."
}
if (-not $SkipManifest) {
    Write-Output "  $BUCKET/stable/$Version/SHA256SUMS and SHA256SUMS.sig"
    Write-Output "  $BUCKET/channel_versions.json (signed)"
}
Write-Output ''

if ($DryRun) {
    Write-Output 'Dry run - nothing uploaded.'
    exit 0
}

# ---------------------------------------------------------------------------
# Upload
# ---------------------------------------------------------------------------

Assert-Credentials
$r2 = Get-R2Credentials

function Invoke-R2Put {
    Param(
        [String]$Key,
        [String]$File,
        [String]$ContentType,
        [String]$CacheControl
    )
    Write-Output "Uploading $Key ..."
    $errorFile = Join-Path $env:TEMP 'mnemos-r2-error.txt'
    $code = curl.exe -s --fail-with-body -o $errorFile -w '%{http_code}' `
        --aws-sigv4 'aws:amz:auto:s3' -H 'x-amz-content-sha256: UNSIGNED-PAYLOAD' `
        --user "$($r2.AccessKeyId):$($r2.SecretKey)" `
        -H "Content-Type: $ContentType" `
        -H "Cache-Control: $CacheControl" `
        -T $File `
        "$($r2.Endpoint)/$BUCKET/$Key"

    if ($code -ne '200') {
        if (Test-Path $errorFile) { Get-Content $errorFile | Select-Object -First 10 | Write-Output }
        throw "Failed to upload $Key (HTTP $code)"
    }
}

# Installer first: the manifest must never advertise a version whose bytes aren't there yet,
# or every running client will fail its download until the second upload lands.
if ($UploadInstaller) {
    New-ReleaseTag -Commit $built.Commit

    # The source archive goes up before anything that names this release. A Mac build of the same
    # commit may have uploaded it already.
    if ((Get-AssetCode -V $Version -Name $SourceName) -eq '200') {
        Write-Output "$SourceName is already in the bucket for $Version."
    } else {
        New-SourceArchive -Commit $built.Commit -OutFile $SourcePath
        Invoke-R2Put -Key "stable/$Version/$SourceName" -File $SourcePath `
            -ContentType 'application/gzip' -CacheControl 'public, max-age=31536000, immutable'
    }

    # Installers are keyed by version and never rewritten, so they can cache forever.
    Invoke-R2Put -Key $installerKey -File $INSTALLER_PATH `
        -ContentType 'application/octet-stream' -CacheControl 'public, max-age=31536000, immutable'
    Write-Output "Uploaded digest, for the run that publishes: -ExpectAsset $InstallerExpect"
    # release.yml hands this to the publish job, which signs nothing it was not told about.
    if ($env:GITHUB_OUTPUT) {
        # No BOM: Windows PowerShell's Add-Content -Encoding utf8 would write one into the key.
        [IO.File]::AppendAllText($env:GITHUB_OUTPUT, "digest=$InstallerExpect`n", (New-Object Text.UTF8Encoding($false)))
    }
}
# The manifest is the one mutable object; a long cache here would delay every release.
#
# -SkipManifest uploads the installer without advertising it. Use it when a release also needs a
# macOS build: the manifest carries one version for every platform, so publishing it from here the
# moment the Windows installer lands would point Macs at a version whose .dmg does not exist yet.
# Upload both platforms first, then publish once with -PublishOnly.
if (-not $SkipManifest) {
    # Last check before the manifest names this version, which also covers -PublishOnly and
    # -Rollback: every asset a client can ask for must be downloadable, this run's own upload
    # included. The installer is already up at this point, so a refusal leaves it there
    # unadvertised and says how to publish later.
    $missing = @()
    $required = @('MnemosSetup.exe', 'Mnemos-arm64.dmg')
    if ($required -notcontains $INSTALLER_NAME) { $required += $INSTALLER_NAME }
    foreach ($asset in $required) {
        $code = Get-AssetCode -V $Version -Name $asset
        if (($code -eq '200') -and -not $script:ExpectedAssets[$asset]) {
            # Served but not vouched for: New-SignedRelease would leave its platform out.
            $code = "200, but no -ExpectAsset $asset=<sha256> from its build"
        }
        if ($code -eq '200') { continue }
        if (($asset -eq 'Mnemos-arm64.dmg') -and $AllowMissingMac) {
            Write-Warning "Publishing $Version without Mnemos-arm64.dmg (HTTP $code). Every Mac install fails its update until that .dmg is uploaded (-AllowMissingMac)."
            continue
        }
        Write-Output "$asset for $Version is not downloadable (HTTP $code)."
        $missing += $asset
    }
    if ($missing.Count -gt 0) {
        Write-Output ''
        if ($UploadInstaller) {
            Write-Output "$INSTALLER_NAME uploaded, manifest NOT published."
        } else {
            Write-Output 'Manifest NOT published.'
        }
        Write-Output "Publish $Version once every asset returns 200:"
        Write-Output "  ./script/windows/release.ps1 -PublishOnly -Version $Version -ExpectAsset <dmg>=<sha256>,<exe>=<sha256>"
        throw "Refusing to publish $Version while $($missing -join ', ') is missing."
    }

    $signedDir = New-SignedRelease -ManifestPath $manifestPath | Select-Object -Last 1
    Invoke-R2Put -Key "stable/$Version/SHA256SUMS" -File (Join-Path $signedDir 'SHA256SUMS') `
        -ContentType 'text/plain; charset=utf-8' -CacheControl 'public, max-age=60'
    Invoke-R2Put -Key "stable/$Version/SHA256SUMS.sig" -File (Join-Path $signedDir 'SHA256SUMS.sig') `
        -ContentType 'text/plain; charset=utf-8' -CacheControl 'public, max-age=60'
    # The manifest goes last: it is what tells clients the release exists.
    Invoke-R2Put -Key 'channel_versions.json' -File $manifestPath `
        -ContentType 'application/json' -CacheControl 'public, max-age=60'
}

# ---------------------------------------------------------------------------
# Prune
# ---------------------------------------------------------------------------

# Every release parks another ~175 MB in the bucket. Without this, storage grows without bound.
# The version format sorts chronologically as text, so the stored versions are ordered newest
# first, the protected ones are set aside, the newest $Keep of the rest stay and everything older
# is deleted. Protected are the version this run published and the one the live manifest
# names. Neither has to be the newest in the bucket: -Rollback publishes an older version, and
# pruning it would leave the manifest naming a version whose bytes were just deleted. Filtering
# them out before counting keeps them on top of $Keep, so that holds for -KeepReleases 1 too.
function Remove-OldReleases {
    Param([int]$Keep, [String[]]$Protect)

    $listUrl = "$($r2.Endpoint)/$BUCKET" + '?list-type=2&prefix=stable/&delimiter=/'
    $body = curl.exe -s --aws-sigv4 'aws:amz:auto:s3' -H 'x-amz-content-sha256: UNSIGNED-PAYLOAD' --user "$($r2.AccessKeyId):$($r2.SecretKey)" $listUrl
    try {
        $xml = [xml]$body
    } catch {
        Write-Warning "  Could not list bucket contents to prune: $_"
        return
    }

    $versions = @($xml.ListBucketResult.CommonPrefixes.Prefix |
            Where-Object { $_ } |
            ForEach-Object { $_.TrimEnd('/') -replace '^stable/', '' } |
            Sort-Object -Descending)

    $candidates = @($versions | Where-Object { $Protect -notcontains $_ })

    if ($candidates.Count -le $Keep) {
        Write-Output "  $($versions.Count) release(s) stored, keeping up to $Keep plus the protected ones - nothing to prune."
        return
    }

    foreach ($old in $candidates[$Keep..($candidates.Count - 1)]) {
        # List and delete every asset under the version, so this stays correct once macOS .dmg
        # files live alongside the Windows installer.
        $objUrl = "$($r2.Endpoint)/$BUCKET" + "?list-type=2&prefix=stable/$old/"
        $objBody = curl.exe -s --aws-sigv4 'aws:amz:auto:s3' -H 'x-amz-content-sha256: UNSIGNED-PAYLOAD' --user "$($r2.AccessKeyId):$($r2.SecretKey)" $objUrl
        try {
            $keys = @(([xml]$objBody).ListBucketResult.Contents.Key | Where-Object { $_ })
        } catch {
            Write-Warning "  Could not list $old to prune it: $_"
            continue
        }

        foreach ($key in $keys) {
            $code = curl.exe -s -o NUL -w '%{http_code}' -X DELETE `
                --aws-sigv4 'aws:amz:auto:s3' -H 'x-amz-content-sha256: UNSIGNED-PAYLOAD' `
                --user "$($r2.AccessKeyId):$($r2.SecretKey)" `
                "$($r2.Endpoint)/$BUCKET/$key"
            if ($code -notin @('200', '204')) {
                Write-Warning "  Failed to delete $key (HTTP $code)"
            }
        }
        Write-Output "  Pruned $old ($($keys.Count) object(s))"
    }
}

Write-Output ''
if ($SkipManifest) {
    # Nothing was published, so this is half a release; the next publish prunes.
    Write-Output 'Not pruning: -SkipManifest published nothing.'
} else {
    $liveVersion = Get-LiveVersion
    if (-not $liveVersion) {
        Write-Warning "Not pruning: could not read the live version from $ManifestUrl, so it cannot be protected."
    } else {
        $protect = @(@($Version, $liveVersion) | Select-Object -Unique)
        Write-Output "Pruning old releases (keeping the newest $KeepReleases besides $($protect -join ' and '))..."
        Remove-OldReleases -Keep $KeepReleases -Protect $protect
    }
}

# ---------------------------------------------------------------------------
# Verify
# ---------------------------------------------------------------------------

# Nothing was published under -SkipManifest, so the manifest still names whatever the last real
# release was. Checking it here would warn about a mismatch that is the expected outcome.
if (-not $SkipManifest) {
    Write-Output ''
    Write-Output 'Verifying published manifest...'
    $publishedPath = Join-Path ([IO.Path]::GetTempPath()) ("mnemos-manifest-" + [Guid]::NewGuid().ToString('N') + '.json')
    try {
        $global:LASTEXITCODE = 0
        & curl.exe -sfL --max-time 20 -o $publishedPath $ManifestUrl
        if ($LASTEXITCODE -ne 0) { throw "curl exit $LASTEXITCODE" }
        Invoke-UpdateSigning verify-manifest --manifest $publishedPath --public-keys $PublicKeysFile --expect-version $Version | Out-Null
        Write-Output "  $ManifestUrl -> $Version, signature OK"
    } catch {
        Write-Warning "  $ManifestUrl does not serve a verified manifest for $Version yet ($_). It may still be caching."
    } finally {
        Remove-Item -Path $publishedPath -Force -ErrorAction SilentlyContinue
    }

    # -PublishOnly and -Rollback build nothing, so the commit for a new GitHub release comes from
    # MNEMOS_RELEASE_COMMIT (set by .github/workflows/release.yml) when there is one.
    $commitForGitHub = if ($UploadInstaller) { $built.Commit } elseif ($env:MNEMOS_RELEASE_COMMIT) { $env:MNEMOS_RELEASE_COMMIT.Trim() } else { '' }
    Publish-GitHubRelease -Dir $signedDir -Commit $commitForGitHub
}

Write-Output ''
if ($SkipManifest) {
    Write-Output "Uploaded $Version ($ARCH). The manifest is untouched, so nothing advertises this version yet."
    Write-Output 'Next, on the Mac: build the .dmg for this version and publish both:'
    Write-Output "  ./script/macos/release --version $Version --expect-asset $InstallerExpect"
    Write-Output 'Or, if that .dmg is already in the bucket, publish from here:'
    Write-Output "  ./script/windows/release.ps1 -PublishOnly -Version $Version -ExpectAsset $InstallerExpect,<the .dmg's>"
} else {
    Write-Output "Released $Version."
    Write-Output 'Running clients pick it up within 10 minutes, or on next app focus.'
}

Write-Output "RELEASE OK $Version"
exit 0
