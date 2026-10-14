# Releasing Mnemos

How a commit becomes an update on every install.

Throughout this file, `$RELEASES` is the releases base URL compiled into official builds
(`releases_base_url`, `https://admin.twincurrent.dk/releases`, set by `app/build.rs`) and
`$SERVER` is the update server behind it (`https://admin.twincurrent.dk`). Credentials and bucket
names come from environment variables, listed under "One-time setup".

## The shape of it

The updater is upstream Warp's (`app/src/autoupdate/`), changed so that it trusts only signed
data. A build has an updater only when it was built as an official release
(`MNEMOS_OFFICIAL_RELEASE=1`, which both release scripts set) or with an explicit
`RELEASES_BASE_URL`; every other build, a plain `script/bundle` included, never updates itself
(`autoupdate_config_json` in `app/build.rs`, and `PACKAGING.md`).

A client with an updater fetches `$RELEASES/channel_versions.json` with a plain anonymous GET (no
account token, no install id), first a minute after the first window has drawn, then every 10
minutes and when the app is activated. It acts only on the manifest's signed payload: the Ed25519
signature must verify against a public key baked into the build, the payload must list a release
for the client's platform (`macos-aarch64`, `windows-x86_64`, ...), and the downloaded file must
match that release's sha256 and size before it is mounted, copied or run. On Windows it then runs
the Inno Setup installer silently (`/SILENT /update=1`); the installer is per-user
(`PrivilegesRequired=lowest`), so an update needs no UAC prompt, and the app relaunches itself on
the new version. On macOS the new app must also be signed by the same team as the running one
(see "Signing" under macOS).

```
  build machine                  $SERVER (update server)           R2 bucket ($MNEMOS_RELEASES_BUCKET)
  -------------                  -----------------------           ------------------------------------
  release script
    ├─ build ──► asset
    ├─ upload ─────────────────────────────────────────────────►  stable/<version>/<asset>
    ├─ upload ─────────────────────────────────────────────────►  stable/<version>/Mnemos-source-<commit>.tar.gz
    └─ sign + upload ──────────────────────────────────────────►  stable/<version>/SHA256SUMS, SHA256SUMS.sig
                                                                   channel_versions.json (signed)
  running client ──GET /releases/channel_versions.json ──┐
                 ──GET /releases/stable/<version>/<asset>┤
                                                         └──302──► bytes come from R2
```

`releases_base_url` deliberately points at the update server rather than straight at R2. It cannot
be changed for installs already in the wild, so it has to be a host the publisher controls
indefinitely; the server's `/releases/*` route (Admin repo, `frontend/app/releases/[...path]`)
302-redirects to the bucket, with a presigned URL when the server has R2 credentials and to
`MNEMOS_RELEASES_ORIGIN` otherwise. Storage stays swappable, and the bytes never transit the server.

**Builds from before signing** (every install older than the signed manifest) ask
`$SERVER/client_version` first and fall back to `$RELEASES/channel_versions.json`. They read only
the unsigned `stable.version` and ignore the `signed` field, so the manifest keeps the legacy
`dev` / `preview` / `stable` fields for them. Two consequences: the manifest may only name a
version once every platform's asset is in the bucket (those builds cannot check anything), and
`MNEMOS_PINNED_VERSION` on the server (see "One-time setup") only reaches them.

## Update signing

Every manifest is signed with an Ed25519 key. The public half is compiled into the app; the secret
half signs `channel_versions.json` and each release's `SHA256SUMS`. The tool is `update_signing`
(`crates/channel_versions/src/bin/update_signing.rs`), the format is described in
`crates/channel_versions/src/signed.rs`.

### Generating the key

The owner does this once, on a machine they trust, before the first signed release:

```sh
cargo run -p channel_versions --bin update_signing -- keygen --secret-out <path outside the repo>
```

`keygen` refuses to overwrite an existing file, writes the secret key (`mnemos-update-sec:...`,
one line, mode 0600 on Unix) to that path and prints the public key line
(`mnemos-update-pub:...`). Add that line to `script/update-signing.pub` and commit it: `app/build.rs`
bakes every key line of that file into a build with an updater, and refuses to build one while the
file lists no key. `MNEMOS_UPDATE_PUBLIC_KEYS` (comma separated) replaces the file, for forks and
test hosts.

### Where the secret key lives

- **CI:** the `MNEMOS_UPDATE_SIGNING_KEY` secret of the protected `release` environment. Only the
  publish step of `release.yml` gets it.
- **A release machine:** `MNEMOS_UPDATE_SIGNING_KEY=mnemos-update-sec:...` in that checkout's
  `.twincurrent-build/.env` (gitignored), or in the environment of the release command.
- **A backup** outside both, so a lost machine does not lose the key.

It never goes in the repository, and **never in the environment of an app build**. Any build script
or proc macro among the roughly thousand crates the app compiles can read the environment it runs
in. The release scripts therefore read the key (and `CLOUDFLARE_API_TOKEN`) into a script variable,
remove both from their own environment before `bundle`, `cargo vendor` or any other cargo command
runs, build `update_signing` once without them, and hand the key only to the `update_signing`
processes that need it (`check-key`, `manifest`, `sign-sums`). Do not export it in a shell profile,
and do not set it when running `script/bundle`, `script/run` or cargo yourself.

`update_signing check-key --public-keys script/update-signing.pub` (with the key in
`MNEMOS_UPDATE_SIGNING_KEY`) says whether a key is one the builds trust; both release scripts run it
before they build when they are going to publish. `update_signing public-key` prints the public line
of the key in the environment.

### Rotation

Clients trust only the keys they were built with. To move to a new key:

1. Generate it, and add its public line to `script/update-signing.pub` next to the old one.
2. Release, still signing with the old key. Every install that takes this release now trusts both.
3. Sign later releases with the new key.
4. Remove the old line once every install has updated past the release from step 2. An install
   that has not yet reached it refuses manifests signed by the new key and needs a manual install.

A leaked key cannot be revoked for installs that already trust it: whoever holds it can sign
manifests those installs accept, until they run a build without it. Rotate as above, as fast as the
installs update.

### Payload formats

The `signed` field always carries a format 1 payload, because installs that read only `signed`
refuse any other format. A later format goes in `signed_next` (a list of envelopes) next to it,
never in its place; `signed::verify_manifest` picks the highest format it reads among the envelopes
that verify. A compile-time assert in `update_signing.rs` stops a `PAYLOAD_FORMAT` bump that would
skip this. The app's `fetch_version` (`app/src/autoupdate/mod.rs`) still reads `signed` only, which
keeps working as long as that slot stays format 1.

### Before the first signed release

Both release machines (Mac and Windows) must run release scripts from a checkout that has signed
manifests, with `MNEMOS_UPDATE_SIGNING_KEY` set. A publish from an older checkout writes a manifest
without `signed`. Builds from before signing still update from it, but every build since then
refuses it (`UnsignedManifest` in `app/src/autoupdate/mod.rs`, logged as a warning, not reported)
and stays on its version until a signed manifest is published again.

## Cutting a release

The manifest carries **one version for every platform**, and a client asks for the asset matching
its own machine. A Mac and a Windows box both run Mnemos, so the manifest may only name a version
once both assets are in the bucket. A release from a desk is therefore two steps: **the machine that
builds first skips the manifest, the one that finishes last publishes it.** Either order works.

Publishing signs only bytes a build vouched for. The asset a run built (or, with `--skip-build` /
`-SkipBuild`, read from disk) is one. The other platform's asset counts only when its sha256 is
passed in: the first run prints the exact `--expect-asset` (Mac) / `-ExpectAsset` (Windows) value
after its upload. Each vouched asset is downloaded from the public URL, exactly as a client would,
and must match that hash (and size) before the manifest and `SHA256SUMS` are signed over it. An
asset that is served but that nobody vouched for is never signed; a run that would publish without
the other platform's digest refuses before it builds.

Windows first:

```powershell
# Windows box: stamps the version, uploads MnemosSetup.exe, publishes nothing.
# Prints: --expect-asset MnemosSetup.exe=<sha256>:<size>
./script/windows/release.ps1 -SkipManifest
```
```bash
# Mac: same version; builds and uploads the .dmg, then signs and publishes both
./script/macos/release --version v0.2026.10.09.10.18.stable_01 \
    --expect-asset MnemosSetup.exe=<sha256>:<size>
```

Mac first:

```bash
# Mac: stamps the version, uploads Mnemos-arm64.dmg, publishes nothing.
# Prints: -ExpectAsset Mnemos-arm64.dmg=<sha256>:<size>
./script/macos/release --skip-manifest
```
```powershell
# Windows box: same version; builds the .exe, then signs and publishes both
./script/windows/release.ps1 -Version v0.2026.10.09.10.18.stable_01 -ExpectAsset Mnemos-arm64.dmg=<sha256>:<size>
```

If both assets are already up, either side publishes without building:
`./script/windows/release.ps1 -PublishOnly -Version <v> -ExpectAsset <dmg digest>,<exe digest>`
(both digests, since this run built neither), or
`./script/macos/release --skip-build --version <v> --expect-asset MnemosSetup.exe=<sha256>`
(the .dmg on disk vouches for itself). Both scripts print these follow-up commands when they exit
under a skipped manifest. Each build takes roughly 12 minutes, most of it LTO.

`git pull` on the second machine before it builds. The two assets are meant to be the same commit;
nothing enforces that, and a version whose `.exe` and `.dmg` disagree is very hard to debug later.

**Neither script publishes a version the other platform cannot download.** Before building, each
checks the other platform's asset for that version (`curl -sIL`, following the server's redirect to
the bucket) and its digest, and refuses to start when either is missing. Right before the manifest
upload it checks every asset again, its own fresh upload included. A refusal at that point says so
(`MnemosSetup.exe uploaded, manifest NOT published`) and prints the command that publishes later.
Consequences worth knowing:

- A bare `./script/windows/release.ps1` or `./script/macos/release` (fresh version, manifest on)
  **always refuses**: a version stamped a second ago cannot have the other platform's asset yet.
  Use the two-step flow above.
- A deliberate single-platform release needs `-AllowMissingMac` (Windows) or
  `--allow-missing-windows` (Mac). It publishes with a loud warning. Builds from before signing on
  the other platform then fail their update until that asset is uploaded; signing-aware builds
  there see no release for their platform and stay put.
- `-DryRun` / `--dry-run` builds and checks, warns about a missing asset and uploads nothing.
- `-PublishOnly` and `-Rollback` get the same checks, so they cannot advertise a version whose
  bytes are gone.

| Platform (signed payload key) | Asset in `stable/<version>/` |
| --- | --- |
| `windows-x86_64` | `MnemosSetup.exe` |
| `windows-aarch64` | `MnemosSetup-arm64.exe` |
| `macos-aarch64` | `Mnemos-arm64.dmg` |

There is no universal `Mnemos.dmg`. Mnemos is arm64-only on macOS: `script/macos/bundle` rejects
`--universal`, and `dmg_name()` in `app/src/autoupdate/mac.rs` runs `uname -m` and asks for
`Mnemos-arm64.dmg` on Apple Silicon. That is the only macOS asset any real client requests. A
platform with no vouched asset gets no entry in the signed payload, so its signing-aware clients
see no update rather than a 404.

Each published version directory also holds `SHA256SUMS` (the sha256 of every signed asset, plus
the source archive when the publishing run has that file on disk; a `-PublishOnly` run, the CI
publish job included, does not) and `SHA256SUMS.sig`, a detached signature by the same key under a different
context string, so neither signature can be replayed as the other. Anyone can check a download with
`shasum -a 256 -c SHA256SUMS --ignore-missing` and the list with
`update_signing verify-sums --in SHA256SUMS --sig SHA256SUMS.sig --public-keys script/update-signing.pub`.
When `MNEMOS_GITHUB_RELEASE_REPO=<owner/repo>` is set (and `gh` is signed in), the publishing run
also creates a GitHub release `release/<version>` with the assets, `SHA256SUMS` and its signature;
a failure there only warns, since the manifest is already live.

### Reusing a build

`-SkipBuild` / `--skip-build` never stamp a new version. They release the artifact already on disk
under the version it was built as: the installer's ProductVersion on Windows, `MnemosVersion` in
the dmg's `Info.plist` on the Mac. Pass a version as well and the two must match, or nothing is
uploaded. The client reports the version compiled into it, so uploading an old build under a new
version makes every client download it, still see itself behind, and reinstall it on every check
until the next real release. That happened with `v0.2026.09.06.19.19`.

### Windows flags

| Flag | Effect |
| --- | --- |
| `-DryRun` | Build and print what would be published; upload nothing. |
| `-SkipBuild` | Upload the installer already in `script/windows/Output` under the version it was built as (its ProductVersion). With `-Version`, the two must match. |
| `-SkipManifest` | Upload the installer (and the source archive) without advertising it. Prints the `-ExpectAsset` / `--expect-asset` value for the run that publishes. |
| `-ExpectAsset <name>=<sha256>[:<size>]` | A digest another run's build produced, as that run printed it. Comma separated or repeated. Nothing is signed without one, except this run's own installer. |
| `-PublishOnly -Version <v>` | Publish the manifest for an already-uploaded version. Needs `-ExpectAsset` for every asset to sign. |
| `-Rollback <version>` | Point the manifest at an older version that is still in the bucket. That version's `SHA256SUMS` and `SHA256SUMS.sig` must be there too: after their signature checks, they vouch for its assets. See "Known gaps" for what a rollback does not do. |
| `-AllowMissingMac` | Publish even though `Mnemos-arm64.dmg` for the version is missing. |
| `-AllowDirty`, `-AllowUnpublished` | See "Corresponding Source". |
| `-KeepReleases <n>` | How many releases to retain in the bucket besides the protected ones. Default 5. |
| `-Patch 02` | Second release within the same minute. |
| `-ARCH arm64` | Build the arm64 installer instead. |

After a successful publish the Windows script prunes the bucket (the Mac script never prunes). It
never prunes under `-SkipManifest`, and it never deletes the version it just published or the
version the live manifest (`MNEMOS_MANIFEST_URL`) names; after a `-Rollback`, both can be older than
everything else. Those two are set aside, the newest `-KeepReleases` of the rest are kept, and older
ones are deleted with everything under them, `SHA256SUMS` included. If the live manifest cannot be
read, nothing is pruned.

Signing the installer: `bundle.ps1` takes `-SIGN_TOOL_CMD` and otherwise falls back to the
`SIGN_TOOL_CMD` environment variable. `release.ps1` has no such parameter; set the variable in the
environment or in `.twincurrent-build/.env` (gitignored). `release.ps1` exports every key of that
file except `CLOUDFLARE_API_TOKEN` and `MNEMOS_UPDATE_SIGNING_KEY`, which it keeps to itself.

### Running detached on Windows

Every run except `-DryRun` ends with one line, `RELEASE OK <version>` or
`RELEASE FAILED: <reason>`, and a matching exit code. To run it in the background (for example from
an agent or over SSH), redirect at the process level:

```powershell
$p = Start-Process powershell -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','script\windows\release.ps1','-SkipManifest' `
    -WorkingDirectory C:\path\to\mnemos `
    -RedirectStandardOutput "$env:TEMP\release-out.log" -RedirectStandardError "$env:TEMP\release-err.log" `
    -WindowStyle Hidden -PassThru
$null = $p.Handle   # Windows PowerShell 5.1 can report a null ExitCode without this
$p.WaitForExit()
$p.ExitCode
```

Redirecting inside PowerShell instead (`*>` or `2>&1` on the script call) is what used to kill
releases seconds in: Windows PowerShell 5.1 turns every stderr line of a native command into a
`NativeCommandError`, which `$ErrorActionPreference = 'Stop'` made fatal. The scripts now run
cargo and ISCC under `Continue` and judge them by exit code, but that path has not been
exercised on 5.1; prefer the recipe above.

## Releasing from CI (`.github/workflows/release.yml`)

Pushing a signed, annotated tag named like the version builds and publishes a release from that one
commit, with the same two scripts a desk release runs:

```sh
cargo run -p channel_versions --bin update_signing -- stamp-version   # prints the version name
git tag -s <version> -m "Mnemos <version>"
git push <public remote> <version>
```

The jobs, in order:

| Job | Runner | Does | Holds |
| --- | --- | --- | --- |
| `verify-tag` | ubuntu | Checks the tag is annotated, well formed, signed by a key in `MNEMOS_TAG_ALLOWED_SIGNERS` (ssh `allowed_signers` lines) and names the pushed commit. | the GitHub token, for one tag fetch |
| `macos` | macos-15 | Builds the .dmg with `release --skip-manifest --dry-run` (uploads nothing) and passes it on as a workflow artifact. | the macOS signing certificate (the build signs the app) |
| `windows` | windows-2022 | Builds the installer with `release.ps1 -SkipManifest -DryRun` and passes it, with its staged NOTICE, as an artifact. | `SIGN_TOOL_CMD`, when set |
| `macos-upload`, `windows-upload` | fresh runners | Compile nothing (`cargo vendor` for the source archive only downloads), upload the artifact with `--skip-build` / `-SkipBuild` and output its `<name>=<sha256>:<size>`. | the R2 token, on the upload step only |
| `publish` | windows-2022 | Builds `update_signing` in a step with no secrets, then runs `release.ps1 -PublishOnly -ExpectAsset <both digests>` after checking that each upload job named its own platform's asset. Creates the GitHub release. | the R2 token and the signing key, on the publish step only; `contents: write` |

The workflow is `contents: read` everywhere else, and every checkout sets
`persist-credentials: false`. The build jobs compile roughly a thousand third-party crates, so they
get no R2 token, no signing key and no write token. One risk remains: the publish job compiles
`update_signing`'s own dependencies (`ring` and others) in an earlier step, without secrets but on
the same runner as the signing step.

All secrets live in the protected `release` environment (Settings > Environments), which should
require a reviewer and allow only release tags:

| Secret | Used by |
| --- | --- |
| `MNEMOS_TAG_ALLOWED_SIGNERS` | `verify-tag`; without it the tag is refused |
| `CLOUDFLARE_ACCOUNT_ID`, `CLOUDFLARE_API_TOKEN` | upload and publish steps |
| `MNEMOS_RELEASES_BUCKET` | optional, defaults to `mnemos-releases` |
| `MNEMOS_UPDATE_SIGNING_KEY` | the publish step |
| `MNEMOS_MACOS_CERT_P12`, `MNEMOS_MACOS_CERT_PASSWORD` | `macos` (base64 .p12 and its password, imported into a temporary keychain) |
| `MNEMOS_MACOS_SIGNING_TEAM` | `macos`, `macos-upload` |
| `MNEMOS_WINDOWS_SIGN_TOOL_CMD` | `windows`, optional |

Every job names the `release` environment, so a reviewer approves six jobs per release. The
publish job sets `MNEMOS_GITHUB_RELEASE_REPO` to the workflow's repository and
`MNEMOS_RELEASE_COMMIT` to the tagged commit, for the GitHub release.

## macOS

macOS cannot be built from the Windows box: `hdiutil`, `codesign`, `plutil` and
`install_name_tool` are macOS-only. The reverse is equally true: the Mac cannot produce the Windows
installer, because Inno Setup's compiler is a Windows binary and the build copies in `conpty.dll`,
`OpenConsole.exe` and the DXC DLLs. Each platform builds its own asset; `release.yml` is the way to
get both from one place.

A Mac that cuts releases needs `script/macos/bootstrap_release` once, after `script/macos/bootstrap`:
it installs `create-dmg`.

```bash
./script/macos/release --skip-manifest            # stamp, build, upload; publish from Windows
./script/macos/release --version <v> --expect-asset MnemosSetup.exe=<sha256>:<size>
                                                  # build <v>'s dmg, upload, check both, publish
./script/macos/release --skip-manifest --nosign   # ad-hoc; the only option over SSH, but
                                                  # team-signed installs refuse it
```

**Run it from a Terminal on the Mac, not over SSH.** Signing needs the login keychain unlocked,
and an SSH session cannot reach it: `codesign` fails with `errSecInternalComponent`, and only when
bundle signs the app, some 20 minutes into the build. The release script probes the signing
identity with a throwaway `codesign` first and refuses up front instead.

If you do drive it over SSH anyway (with `--nosign`), export a real PATH first. A non-interactive
SSH shell does not source `.zshrc`, so Homebrew's `create-dmg` is missing and the build dies *after*
compiling, during bundling:

```bash
export PATH="/opt/homebrew/bin:/opt/homebrew/sbin:$PATH"
```

**The dmg's Finder layout can fail; bundle recovers on its own.** After a full compile,
`create-dmg` runs an AppleScript that asks Finder to lay out the dmg window (icon positions,
background image). A shell descended from Mnemos or Claude Code has no TCC grant to send Apple
events to Finder, so that step fails with `Not authorised to send Apple events to Finder. (-1743)`
and `create-dmg` exits 64. `script/macos/bundle` then deletes the half-built image and runs
`create-dmg` once more with `--skip-jenkins`, which skips the Finder step; the layout is cosmetic
and the updater does not care. `MNEMOS_DMG_SKIP_FINDER=1` (or `RUNNER_NAME=nsc-*`) skips it from
the start. This used to kill the release after the compile (2026-09-14 and 2026-09-24).

**Signing.** `--selfsign` (the default) signs with a local *Apple Development* certificate;
`--nosign` seals the bundle ad-hoc. Both use `script/Release-Entitlements.plist`, which leaves out
`get-task-allow` (with it any process of the user's could attach to Mnemos and use its camera,
microphone and keychain grants); only `script/macos/run` signs dev bundles with
`script/Debug-Entitlements.plist`, and `release` refuses a dmg that carries `get-task-allow`. Neither is notarized, and nothing in this repo notarizes: real
Developer ID signing needs `MNEMOS_DEVELOPER_ID_CERT`, `MNEMOS_DEVELOPER_ID_CERT_PASSWORD`,
`MNEMOS_NOTARIZATION_APPLE_ID` and `MNEMOS_NOTARIZATION_PASSWORD`, which `bundle` reads from GCP
Secret Manager in upstream Warp's setup; that path is not wired up here, and releases so far are
signed with Apple Development certificates only. So on a first, manual install of the
un-notarized dmg Gatekeeper demands a one-time right-click, Open, or
`xattr -dr com.apple.quarantine /Applications/Mnemos.app`. The updater itself checks signature
integrity and the signing team, not notarization.

The updater accepts a new build only when its signature verifies and comes from the **same team as
the running app** (`verify_code_signature` in `app/src/autoupdate/mac.rs`); a team-signed install
refuses an ad-hoc update. A keychain can hold valid Apple Development identities for more than one
team, so the team is pinned rather than left to keychain order:

| Variable | Effect |
| --- | --- |
| `MNEMOS_MACOS_SIGNING_IDENTITY` | Identity to sign with. Use its SHA-1 hash from `security find-identity -p codesigning -v`; a name only works while it matches exactly one valid identity. Unset, the first *Apple Development* identity listed is used, with a warning when there are several. |
| `MNEMOS_MACOS_SIGNING_TEAM` | Required TeamIdentifier. `bundle --selfsign` fails if the signed app has another one (or none), and `release` refuses a dmg with another team, `--skip-build` included. Under `--nosign` it only warns. |
| `MNEMOS_SELFSIGN_STRICT` | `1` makes a missing identity an error in `bundle --selfsign` instead of an ad-hoc fallback. `release` sets it whenever it builds in selfsign mode. |

Set them in the environment or in `.twincurrent-build/.env` (gitignored). Which team to pin is a
deliberate choice: installs already signed by one team refuse updates from the other. In selfsign
mode `release` also refuses an ad-hoc dmg outright. The team is always read from the signature
(`codesign -dv`, `TeamIdentifier=`); the ten characters in parentheses in an identity's name are
not the team.

### Two build inputs the Mac resolves for you

Both used to fail late and cryptically. `script/macos/bundle` now handles each before it can
bite, but it is worth knowing what they are.

**The Metal compiler.** Xcode 26 ships `xcrun metal` as a stub and keeps the real compiler in a
separately installed *MetalToolchain* component. An Xcode point upgrade bumps the build it
demands and orphans the one on disk:

```
$ xcodebuild -showComponent MetalToolchain
Build Version: 17F109
Status: uninstalled
```

`crates/mnemos_ui/build.rs` shells out to `xcrun metal`, so this would panic the build with
`cannot execute tool 'metal' due to missing Metal Toolchain`. The documented fix,
`xcodebuild -downloadComponent MetalToolchain`, fetches its catalog from `gdmf.apple.com`, which
is also a DEP/MDM check-in endpoint and is null-routed in `/etc/hosts` on machines that avoid
device enrollment, so it dies with `Failed fetching catalog for assetType`.

`script/macos/metal_toolchain` sidesteps both. The previously downloaded asset is still under
`/System/Library/AssetsV2` as a signed cryptex image that mounts read-only **without root**; the
script mounts it in the per-user temp dir and writes an `xcrun` shim into `target/metal-shim` that
routes only `metal` and `metallib` at it. A `.metallib` from the older toolchain is accepted by the
newer Xcode. Nothing is downloaded and `/etc/hosts` is not touched. If no asset exists at all the
script says so and tells you what to run.

**The dock tile plugin.** `MnemosDockTilePlugin.docktileplugin` is built from its source in
`app/DockTilePlugin`: `app/build.rs` runs its Makefile on every macOS build of the app and leaves
`target/<profile>/MnemosDockTilePlugin.docktileplugin`, which `script/macos/bundle` copies in
(with `--skip-build` and no build output it runs the Makefile itself). Nothing is
downloaded any more: an AGPL release ships only what its Corresponding Source builds. The old
prebuilt tarball under `$RELEASES/prebuilt/` and `MNEMOS_DOCK_TILE_PLUGIN_URL` are unused; the
tarball is Warp's binary and should be deleted from the bucket.

## Corresponding Source

Every release is an AGPL distribution, so its source offer (the About page and the NOTICE in each
installer) has to work for anyone. Both release scripts enforce that before they upload:

- **Clean tree.** Uncommitted changes and untracked, non-ignored files both refuse; untracked files
  under `resources/bundled` would otherwise ride into the bundle. With `--skip-build` /
  `-SkipBuild` the scripts read the commit and the "NOT a release build" marker from the NOTICE
  the build bundled (inside the .dmg on the Mac, the staged `target\<triple>\rlto\resources\NOTICE`
  on Windows) and hold it to the same rule. `--allow-dirty` / `-AllowDirty` overrides; never
  publish such a build.
- **Published commit.** The commit must be on the public repository named in `script/source-url`
  (or `MNEMOS_SOURCE_URL`), fetched anonymously into `refs/mnemos-public/*`, so a private repository
  never passes. `--allow-unpublished` / `-AllowUnpublished` is for builds that never leave
  TwinCurrent; their NOTICE says so.
- **Tag.** The commit gets a local annotated tag `release/<version>-macos` or
  `release/<version>-windows`. Nothing is pushed: push the tags to the public repository. The
  version stamp also refuses a new version that would not sort after these tags and the live
  manifest's version.
- **Source archive.** `Mnemos-source-<commit>.tar.gz` (`git archive` of the commit plus
  `cargo vendor` of every Rust dependency, with `vendor-config.toml` for
  `cargo build --config vendor-config.toml`) is uploaded to `stable/<version>/` next to the
  installers, unless the other platform already uploaded it for the same commit. It is what keeps a release buildable if a `warpdotdev/*` git dependency disappears.
  Pruning old versions deletes it with them.

## Version strings are not cosmetic

`v0.<yyyy.MM.dd.HH.mm>.stable_<nn>`, for example `v0.2026.10.09.10.18.stable_01`, stamped in UTC
by `update_signing stamp-version`.

The client parses the embedded timestamp out of this string to order releases
(`crates/channel_versions/src/lib.rs`). That ordering is the only thing stopping a client from
"updating" onto an older build: a client that finds itself ahead of the advertised version refuses
to move. Break the format and every version comparison silently fails open. Both release scripts
refuse to publish a string they can't order.

Releases before the switch to UTC were stamped in local time, up to two hours ahead of UTC. So
`stamp-version` refuses a version that would not sort after the live manifest's version or any
local `release/*` tag, and says how long to wait. The first release after the switch may have to
wait that long.

## One-time setup

### 1. R2 bucket

Create an R2 bucket. Note its name; the release scripts default to `mnemos-releases`. They upload
through R2's S3 endpoint, so any S3-compatible bucket with the same layout works, as long as
`MNEMOS_PUBLIC_RELEASES_URL` serves it.

### 2. Release machine

Run `script/macos/bootstrap_release` on a Mac that cuts releases. In `.twincurrent-build/.env`
(gitignored) on each release machine:

```
CLOUDFLARE_ACCOUNT_ID=<Cloudflare account id>
CLOUDFLARE_API_TOKEN=<R2 API token, "Object Read & Write", scoped to the bucket>
MNEMOS_RELEASES_BUCKET=<bucket name>
MNEMOS_UPDATE_SIGNING_KEY=<mnemos-update-sec:..., see "Update signing">
```

Both release scripts read this file; real environment variables win over it. Optional:

| Variable | Default | Effect |
| --- | --- | --- |
| `MNEMOS_PUBLIC_RELEASES_URL` | `https://admin.twincurrent.dk/releases` | Where the scripts check and download assets, as clients do. |
| `MNEMOS_MANIFEST_URL` | `$MNEMOS_PUBLIC_RELEASES_URL/channel_versions.json` | The live manifest: read for the version stamp and pruning, verified after publishing. |
| `MNEMOS_UPDATE_PUBLIC_KEYS` | `script/update-signing.pub` | Comma-separated public keys, instead of the file. |
| `MNEMOS_GITHUB_RELEASE_REPO` | unset (no GitHub release) | `owner/repo` to publish a GitHub release to. |
| `MNEMOS_RELEASE_COMMIT` | unset | The commit a `-PublishOnly` run's new GitHub release points at. |
| `MNEMOS_SOURCE_URL` | `script/source-url` | The public source repository. |

### 3. Update server environment

On the server behind `$SERVER` (Admin repo, `frontend/lib/mnemos-releases.ts`): `R2_ACCESS_KEY_ID`,
`R2_SECRET_ACCESS_KEY` and `CLOUDFLARE_ACCOUNT_ID` let `/releases/*` redirect to presigned URLs, so
the bucket can stay private. Without them it redirects to `MNEMOS_RELEASES_ORIGIN=<public bucket
origin, no trailing slash>`.

Optionally `MNEMOS_PINNED_VERSION=<version>` pins builds from before signing to a specific version:
it overrides the manifest that `/client_version` serves and marks it as a rollback, so those clients
move backwards. It does not change `channel_versions.json`, so it has no effect on signing-aware
builds. Unset it to resume normal releases.

### 4. First install is manual

Auto-update only works from a build that has an updater compiled in: an official release with a
signing key. Any install predating that, and any build from source, never checks. Install one
release by hand; everything after that arrives on its own.

## Verifying a release landed

```bash
curl -sL $RELEASES/channel_versions.json -o /tmp/cv.json
cargo run -p channel_versions --bin update_signing -- verify-manifest \
    --manifest /tmp/cv.json --public-keys script/update-signing.pub --expect-version <version>
curl -sIL -o /dev/null -w '%{http_code}\n' $RELEASES/stable/<version>/MnemosSetup.exe    # expect 200
curl -sIL -o /dev/null -w '%{http_code}\n' $RELEASES/stable/<version>/Mnemos-arm64.dmg   # expect 200
curl -sIL -o /dev/null -w '%{http_code}\n' $RELEASES/stable/<version>/SHA256SUMS         # expect 200
```

On Windows use `curl.exe` and `-o NUL`. `-L` matters: the server's `/releases/*` route answers 302
for any path, missing assets included, and only the bucket behind the redirect says 404. Both
release scripts run the manifest check themselves after publishing.

On the client, `mnemos.log` records the manifest fetch (`Fetching the release manifest from ...`)
and what it received. Failed silent installs leave `mnemos_update.log` in the log directory.

## Known gaps

- **No rollback for clients that are ahead.** `-Rollback` republishes an older version but never
  sets `is_rollback` in the signed payload, so a signing-aware client already on a newer version
  stays where it is. Builds from before signing can be moved back with `MNEMOS_PINNED_VERSION`.
- **Unsigned Windows installer.** SmartScreen warns on manual install. Silent auto-updates are
  unaffected (no prompt), so this only bites first-time installs. Fixing it needs a real code
  signing certificate (an Authenticode cert from a CA, which costs money and requires identity
  verification), so it can't be scripted away. Once one exists, `bundle.ps1` takes `-SIGN_TOOL_CMD`
  or falls back to the `SIGN_TOOL_CMD` environment variable; `release.ps1` has no such parameter,
  so set the variable (see "Windows flags"). The Mac dmg is not notarized either; see "Signing"
  under macOS.
- **Update hints are opt-in per release.** Signing-aware clients take `update_by`, `soft_cutoff`
  and `last_prominent_update` from the signed payload only (`PlatformRelease::apply_to`) and ignore
  the unsigned legacy fields. `update_signing manifest` signs them in from `--update-by`,
  `--soft-cutoff` and `--last-prominent-update` (and also writes the legacy fields for older
  builds), and both release scripts pass those flags only when `MNEMOS_UPDATE_BY`,
  `MNEMOS_SOFT_CUTOFF` or `MNEMOS_LAST_PROMINENT_UPDATE` is set (in CI, from the repository
  variables of the same names). A release published without them sends no hints.
- **No Linux release path.** `app/src/autoupdate/linux.rs` expects the same bucket layout with
  Linux asset names, but no script publishes them. Linux is untested.
- **No changelog.** `ChannelVersions.changelogs` is optional and we omit it, so the in-app
  changelog stays empty.
