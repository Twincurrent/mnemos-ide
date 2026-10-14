# Packaging Mnemos IDE

Notes for anyone who builds Mnemos IDE from source to ship it: a distribution package, a Homebrew
cask or formula, a winget manifest, or an internal build.

## Updates

A build from source does not update itself. `app/build.rs` gives a build an updater only when one
of these is set at build time:

- `MNEMOS_OFFICIAL_RELEASE=1`: what `script/macos/release` and `script/windows/release.ps1` set
  for TwinCurrent's own releases. The updater then reads `https://admin.twincurrent.dk/releases`.
- `RELEASES_BASE_URL=<url>`: a fork that runs its own release host.

Either way the build also needs at least one update signing public key, from
`script/update-signing.pub` or from `MNEMOS_UPDATE_PUBLIC_KEYS` (comma separated). Without one the
build stops with an error rather than shipping an updater that accepts nothing.

If you ship updates through your own channel, leave both unset, or set `MNEMOS_UPDATER=off`. The
same variable also turns the updater off at run time, for example from a wrapper script:

```sh
MNEMOS_UPDATER=off mnemos
```

A build without an updater makes no update or changelog request at all and hides the update menu
items.

## What an updater does

It fetches `<releases_base_url>/channel_versions.json` with a plain GET (no account token, no
install id), at most every ten minutes and never earlier than a minute after launch. It acts only
on the manifest's signed payload: the Ed25519 signature must verify with a baked-in key, the
payload must list a release for the running platform (`<os>-<arch>`, for example
`macos-aarch64` or `windows-x86_64`), and the downloaded file must match that release's sha256 and
size before it is mounted, moved or run.

## Running your own release host

1. Generate a key pair:
   `cargo run -p channel_versions --bin update_signing -- keygen --secret-out <file outside the repo>`.
   Put the printed public key line in `script/update-signing.pub` (or `MNEMOS_UPDATE_PUBLIC_KEYS`).
2. Build with `RELEASES_BASE_URL=<your host>`.
3. Serve `stable/<version>/<asset>` and a `channel_versions.json` written by
   `update_signing manifest` with `MNEMOS_UPDATE_SIGNING_KEY` set to the secret key. The release
   scripts do all of this against any S3-compatible bucket; point them at yours with
   `MNEMOS_RELEASES_BUCKET`, `MNEMOS_PUBLIC_RELEASES_URL` and `MNEMOS_MANIFEST_URL`.

## Versions

Release versions are machine versions that order updates: `v0.<yyyy.MM.dd.HH.mm>.stable_<nn>`,
stamped in UTC. The About page shows a semver display version instead: the crate version with the
stamp as build metadata (`0.1.0+2026.10.09.12.30.stable.01`), or `+source.<commit>` for a build
without `GIT_RELEASE_TAG`. Use the display version, or the crate version, for package versions.

## Verifying downloads

Every release directory has `SHA256SUMS` and `SHA256SUMS.sig`. Check a download with
`shasum -a 256 -c SHA256SUMS --ignore-missing`, and the list itself with
`update_signing verify-sums --in SHA256SUMS --sig SHA256SUMS.sig --public-keys script/update-signing.pub`.

## Platforms

Published builds are macOS 11.0 or later on Apple silicon and Windows 11 on x64. Other platforms
build from source but get no published release, so their updater stays idle.

NixOS x86_64 has a source package in `flake.nix` (`nix build`, `nix run`). It installs the desktop
entry and icons, embeds release assets, and disables the self-updater so Nix owns upgrades.
See [docs/nixos.md](docs/nixos.md) for installation and package maintenance.

## File associations (macOS)

Every document type in the bundle's Info.plist is ranked `Alternate`: installing Mnemos never makes
it the default app for folders, scripts or text files. Making it the default terminal from
Settings saves the previous handler, and Settings offers to restore it.
