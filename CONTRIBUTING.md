# Contributing to Mnemos IDE

Thanks for taking the time. Mnemos IDE is maintained by two people, so small, focused changes
with a clear reason get reviewed fastest. For anything large, open an issue first so we can agree
on the approach before you spend the time.

## Where things live

- `ide/`: the Mnemos IDE crates (`mnemos-ide-*`) and the Claude Code plugin. Most new features
  belong here.
- `app/` and `crates/`: upstream Warp code, rebranded. Keep edits here small, and mark each one
  with a `Mnemos IDE:` comment so upstream merges can find it.
- `ARCHITECTURE.md` explains how the pieces fit; `AGENTS.md` has the detailed contributor notes.
  It is also the guide coding agents read (Codex, Gemini CLI and OpenCode load it directly, and
  `CLAUDE.md` imports it for Claude Code), so keep agent instructions there.

## Build

See "Build from source" in `README.md` for prerequisites. In short:

```sh
./script/bootstrap    # once per machine
./script/run          # dev build and run, on its own data profile
```

On Windows run the scripts from Git Bash. A debug build keeps its own data apart from an installed
Mnemos ("Dev builds keep their own data" in `README.md`).

Some dependencies are forks in git repositories that Warp hosts (`git =
"https://github.com/warpdotdev/..."` in the Cargo.toml files), and a few follow a branch. Build
with the committed `Cargo.lock` (`--locked`) and do not run a bare `cargo update`: it would move
them to whatever those branches point at now. The source archive of each release (see `NOTICE`)
contains the exact revisions, and is the canonical source for them.

## Test

Before you open a pull request, run these and make sure they pass. CI runs the same on every pull
request (`.github/workflows/pr.yml`), on macOS, Windows and Linux (the Linux leg is advisory).

```sh
# Type-check the app crate and its tests
cargo check -p mnemos --features gui --lib --tests

# The gate for the Mnemos IDE layer: tests and clippy (-D warnings) for every
# ide/mnemos-ide-* crate, then script/oss/grep-checks.sh, the leak check and cargo-deny
script/ide-check

# The text checks alone (no build): install sources, domains and artwork Mnemos does not own,
# and billing or upsell copy
script/oss/grep-checks.sh
```

`script/ide-check` runs every step even when an earlier one fails, and exits non-zero if any did.
It skips cargo-deny when it is not installed (`cargo install --locked cargo-deny`), and
`MNEMOS_SKIP_ADVISORIES=1` skips the advisory step, which needs the network. Upstream's
`script/presubmit` does not pass in this fork; `script/ide-check` is the gate. If you change
upstream code under `app/` or `crates/`, also run the tests of the crates you touched
(`cargo test -p <crate>`).

### Tests and your HOME

The tests run against your `HOME`. The one that reads real Claude Code transcripts
(`mnemos-ide-sessions` parsing `~/.claude/projects`) only runs with `MNEMOS_TEST_REAL_CLAUDE=1`.
To keep every test off your files, run the check with a scratch `HOME` and point cargo and rustup
back at the real toolchain, as CI does:

```sh
CARGO_HOME="$HOME/.cargo" RUSTUP_HOME="$HOME/.rustup" HOME="$(mktemp -d)" script/ide-check
```

Unit tests of upstream crates that never pick a channel (builds with the `test-util` feature)
keep `settings.toml` and the app data directory in a per-process temp directory
(`mnemos-unit-test-home-<pid>`), not in your real config.

### Leak check and test fixtures

`script/oss/leak-check.sh` fails on private data (home paths, private names, credentials) in
tracked files, and runs gitleaks when it is installed. It reads gzip files too: they are
inflated and scanned like any other file. Any other compressed or archive format (zip, xz,
bzip2, zstd, 7z) fails the check unless its path is added to `COMPRESSED_ALLOWLIST` in that
script, with a comment saying why its contents are safe. Prefer plain or gzip fixtures.

## Pull requests

1. Fork the repository and create a branch from `main`.
2. Make your change, with tests where it makes sense. Keep unrelated cleanups out of it.
3. Run the checks above, and sign off every commit (below).
4. Write commit messages in the imperative ("Add X", "Fix Y") and explain the why in the body
   when it is not obvious.
5. Open a pull request against `main` and fill in the template. Link the issue it fixes, if any.

We may ask for changes or split a large pull request into smaller ones. A pull request that has
had no response from its author for a month may be closed; it can always be reopened.

## Sign off your commits (DCO)

Every commit must be signed off under the
[Developer Certificate of Origin](https://developercertificate.org) 1.1:

```sh
git commit -s
```

This adds a `Signed-off-by: Your Name <you@example.com>` line, which certifies that you wrote the
change or otherwise have the right to submit it under the project's license. The DCO check
(`.github/workflows/dco.yml`) runs on every pull request and fails when a commit's sign-off does
not carry the commit's author email (merge commits are skipped). It runs the checker
`script/oss/check-dco.sh` from the base branch, not from your pull request. To check locally:

```sh
script/oss/check-dco.sh origin/main HEAD
```

`git rebase --signoff main` adds the line to commits you have already made.

## License of contributions

Contributions are accepted under the license of the files they change (inbound = outbound):

- **AGPL-3.0-only** for almost everything, the `ide/` crates included (`LICENSE-AGPL`).
- **MIT** for the UI framework crates `crates/mnemos_ui_core` and `crates/mnemos_ui`
  (`LICENSE-MIT`), as upstream.
- **AGPL-3.0-only and MIT** for `ide/claude-plugin`, which is offered under both
  (`ide/claude-plugin/plugins/mnemos-agents/LICENSE`); a contribution there is accepted under
  both.

See `NOTICE`. Do not add code you cannot license that way, and note the license of any new
third-party dependency in your pull request; cargo-deny (`deny.toml`) checks the licenses of Rust
dependencies.

## Reporting bugs and security issues

- Bugs and feature ideas: open an issue using one of the templates.
- Security vulnerabilities: do **not** open a public issue. Follow `SECURITY.md`.

## Code of conduct

Everyone taking part is expected to follow the [Code of Conduct](CODE_OF_CONDUCT.md).
