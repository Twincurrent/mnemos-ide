# Mnemos IDE

**An open-source terminal where Claude Code sessions survive restarts and every running agent is
one click away.**

Mnemos IDE (the app is called **Mnemos**) is a fork of the open-source
[Warp terminal](https://github.com/warpdotdev/warp). The terminal itself, the block-based UI, the
GPU renderer, the editor and the UI framework are Warp's work,
published by Denver Technologies, Inc. under the AGPL. Thank you, Warp. Mnemos adds a layer for people who run Claude Code and other
agent CLIs all day: an agents panel, session restore across restarts, a git tree, and optional
sync between a Mac and a Windows PC.

No account is needed: the terminal, the agents panel and session restore work signed out and
offline. There is no telemetry. Sync is an opt-in layer on top.

Not affiliated with or endorsed by Anthropic or Warp. "Warp" is a trademark of Denver
Technologies, Inc.

## Try it on one machine

A few minutes once Mnemos is built (see [Install](#install)); you need
[Claude Code](https://docs.claude.com/en/docs/claude-code) and `jq`.

1. Install the Claude Code plugin that reports sessions to Mnemos. From your checkout:

   ```sh
   ./script/install-claude-plugin
   ```

   This registers `ide/claude-plugin` as the `mnemos` marketplace, installs
   `mnemos-agents@mnemos` and enables it in `~/.claude/settings.json`. Without a checkout, the
   same from any terminal:

   ```sh
   claude plugin marketplace add https://github.com/Twincurrent/mnemos-ide.git --sparse .claude-plugin ide/claude-plugin
   claude plugin install mnemos-agents@mnemos
   ```

2. Open Mnemos, open a project folder and start `claude` in a terminal tab. The agents panel lists
   the session with its status.
3. Quit Mnemos and open it again. The session comes back in the same pane.

**How restart survival works.** When Mnemos quits (a normal quit, a restart for an update, or a
Windows shutdown or logoff), it records which Claude Code session runs in which pane; on the next
launch the panes, their working directories and their earlier command blocks come back from
Mnemos's local database, and each recorded session is started again in its own pane with
`claude --resume <session id>`. The `claude` process does not keep running across the restart:
the conversation continues from its transcript, and the restored scrollback is a saved copy.

Limits: Claude Code only, and only sessions the plugin has reported. At most 12 sessions are
resumed, a record older than 24 hours is dropped, and nothing is recorded when the app crashes.
It needs the "restore session" setting (`general.restore_session`, on by default);
`MNEMOS_NO_AGENT_RESTORE=1` turns the resume off.

A second machine needs sync; see [Sync](#sync-honest-status) and
[docs/second-machine.md](docs/second-machine.md).

## Features

- **Agents panel.** Every running agent session (Claude Code, Codex, Gemini CLI and OpenCode),
  grouped by project. Click a row to jump to its tab, cancel a Claude session, and see when a
  turn is still running background tasks. Claude Code status comes from the `mnemos-agents`
  plugin above. The Gemini CLI and OpenCode extensions send events without this terminal's
  token, so their status needs the opt-in setting
  `agents.third_party.accept_untokened_cli_agent_events`.
- **Sessions.** The panel lists every project's past Claude Code sessions and resumes one (click)
  or forks it (right-click) in a terminal tab inside that project's tab.
- **Session restore** across restarts, described above.
- **Projects as tabs.** Every workspace tab is a project, and each machine finds its own checkout
  of a project by git remote or folder name.
- **Git tree with pull requests.** Branches, remotes, tags, open PRs with CI status (through the
  `gh` CLI) and the commit graph, in the code-review header. Checkout always refuses rather than
  discarding uncommitted work.
- **Copy as Markdown** for terminal blocks, in place of Warp's "Share block". Nothing is uploaded.
- **Themes.** Mnemos Night (default), Mnemos Day, Mnemos Contrast and Mnemos Moss; five image
  themes with a bundled background (Stone, Slate, Mountain Night, Mountain Dawn, Fjord); twelve
  community themes built from their projects' published palettes (Catppuccin Latte and Mocha,
  Dracula, Gruvbox Dark and Light, Kanagawa, Nord, One Dark, Rosé Pine, Solarized Dark and Light,
  Tokyo Night).
- **With sync on:** projects, settings, Claude Code memory, `~/.claude` config and session
  transcripts follow you between machines, with working directories rewritten per machine, so
  `claude --resume` picks up a session started on the other computer. The agents panel also lists
  the sessions running on your other machines.
- **Remote Control.** A session running on another machine that has Claude Remote Control turned
  on gets an `open` button, which opens its claude.ai Remote Control page in your browser. Remote
  Control is Anthropic's feature: it needs a Claude account and a plan that supports it.
- Everything Warp's terminal already does: blocks, an editor with vim mode, LSP, completions and
  more.

Session restore, the sessions list and transcript sync work with Claude Code only for now.

## Platforms

| Platform | Status |
| --- | --- |
| macOS 11 or later, Apple silicon (arm64) | Supported, used daily |
| Windows 11 x64 | Supported, used daily |
| macOS Intel (x86_64) | Not built; releases are arm64 only |
| Windows arm64 | The installer script has an arm64 target; not tested |
| Linux | Code paths exist upstream; untested, no packages published |

## Install

Signed downloads are coming: there are no public release downloads yet, and no Homebrew or winget
package. Until then, build from source. A build from source does not update itself; see
`PACKAGING.md`. Once releases exist, each one has `SHA256SUMS` and a signature over it, and
`PACKAGING.md` ("Verifying downloads") shows how to check them.

### Build from source

The Rust toolchain is pinned in `rust-toolchain.toml`; `rustup` installs it on the first build.
A first build compiles a large dependency graph and takes a while. Node.js is not needed to
build.

#### macOS (arm64)

Prerequisites: Xcode from the App Store, [Homebrew](https://brew.sh) and
[rustup](https://rustup.rs). `./script/bootstrap` (which runs `script/macos/bootstrap`) installs
the rest, including `protoc` (Homebrew `protobuf`, needed by `crates/remote_server`'s build
script) and `git-lfs`.

```sh
./script/bootstrap
./script/run                # dev build: bundles target/debug/bundle/osx/Mnemos.app and runs it
./script/run --dont-open    # build the bundle without launching it
```

Xcode 26 ships the Metal shader compiler as a separate component. When it is missing, the build
mounts a cached copy on its own (`script/macos/metal_toolchain`); the permanent fix is
`xcodebuild -downloadComponent MetalToolchain`.

#### Windows 11 (x64)

Prerequisites: [Git for Windows](https://gitforwindows.org) (the scripts are bash; run them from
Git Bash) and `winget`. `./script/bootstrap` runs `script/windows/bootstrap.ps1`, which installs:

- the Visual Studio 2022 Build Tools with the C++ workload (the MSVC linker and Windows SDK), when
  they are missing;
- rustup with the MSVC toolchain (then asks you to open a new terminal and run it again);
- CMake, `jq`, `protoc` (`Google.Protobuf`) and Inno Setup (only needed to build an installer);
- turns on `git lfs` and sets `git config --global core.longpaths true`.

Build paths are long, so Windows long paths must be on. The bootstrap checks and tells you; to
turn them on, from an elevated PowerShell:

```powershell
Set-ItemProperty -Path 'HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem' -Name LongPathsEnabled -Value 1
```

```sh
./script/run                                  # dev build and run
./script/bundle -CHANNEL stable -ARCH x64     # installer (script/windows/bundle.ps1)
```

#### Linux

Untested. `./script/bootstrap` runs `script/linux/bootstrap`, which installs the build, runtime
and test dependencies with apt (dnf and pacman lists exist and are untested), including `protoc`
25.1 and `git-lfs`. Reports and fixes are welcome.

### Dev builds keep their own data

A debug build (`./script/run`, or a plain `cargo run`) runs on its own data profile, `dev`,
unless you set `MNEMOS_DATA_PROFILE` yourself. The profile gives it its own sync login, its own
app state and config directory (on macOS `~/.mnemos-dev` instead of `~/.mnemos`), and its own
Mnemos IDE store (projects, tabs, agent sessions) under `Mnemos-dev` in the platform data
directory. So a dev build does not touch an installed Mnemos.

- `MNEMOS_DATA_PROFILE=` (empty) opts out and shares the installed app's data.
- Release-profile builds (`--release`, `release-lto`) ignore the profile for the app's own state
  on macOS, so they share more with an installed Mnemos. `AGENTS.md` ("How to build & run") has
  the full rules.
- A source build can talk to a sync server of your choice:
  `MNEMOS_SERVER_ROOT_URL=http://localhost:8080 ./script/run`. Release builds ignore it.

Cutting a release needs more setup (`script/macos/bootstrap_release`, signing keys); see
`RELEASING.md`.

## How it differs from Warp

Mnemos IDE was forked from Warp's public repository at commit `d0f045c01` (2026-04-28). It is a
hard fork: it does not merge Warp's main branch, and fixes from upstream are ported selectively,
one change at a time. Our edits inside upstream files carry a `Mnemos IDE:` comment so a port can
find them. On top of the Mnemos additions above, the main changes are:

- **Telemetry and crash reporting.** The RudderStack telemetry client is only compiled with the
  non-default `telemetry` cargo feature and has no destination in Mnemos builds; both privacy
  toggles default to off. The Sentry crash reporter is not compiled into release bundles.
- **Network.** Warp's server URLs are replaced by the Mnemos sync server's, which serves none of
  Warp's cloud features. Signed out, nothing is requested before the first window draws.
- **Warp's built-in AI.** The client for Warp's built-in (Oz) agent and the passive suggestions
  are not compiled into Mnemos builds (cargo features `builtin_agent` and `passive_suggestions`,
  both off), so prompts and provider keys entered there go nowhere. Voice input is removed.
  Agent CLIs you run in the terminal talk to their provider directly; Mnemos does not proxy them.
- **Removed:** billing and upsell pages, server experiments, the Firebase sign-in, block sharing
  (replaced by Copy as Markdown), computer use, Warp's artwork and built-in themes, and the
  channels other than stable.
- **Updates.** A release build checks an anonymous, signed, static manifest; a source build has no
  updater.

`docs/upstream-diff.md` lists the removals and disconnections with the commits that made them,
and how to reproduce the diff yourself. `NOTICE` has the full attribution.

## Sync: honest status

Sync is optional. Without signing in, nothing is synced.

- **The server is not open source.** Today the only sync server is the one TwinCurrent runs. Its
  source is not public and is not released under any licence. Self-hosting is planned; until the
  source is published, Mnemos IDE does not call its sync server open or self-hostable.
- **Accounts.** Signing in needs an account on that server, which is not open to the public yet.
- **Encryption is off by default.** Without it, synced data is sent over HTTPS and stored readable
  by whoever runs the server. Settings > Sync can turn on end-to-end encryption with a recovery
  key: the server then stores ciphertext, but still sees key names, sizes, versions and times.
  The recovery key is kept in the OS keychain (a DPAPI-encrypted file on Windows) and is never
  sent; if you lose it, nobody can recover the data. Team data and the live agents feed stay
  readable by the server either way.
- **Secrets.** `~/.claude/.env` syncs only on machines where you turn it on, the `env` and
  `headers` of MCP server definitions are not synced, and secret-looking `env` entries in Claude
  Code's `settings.json` never leave the machine.
- **Synced config can run code.** Hooks sync is off by default on each machine. Every incoming
  change to hooks, commands, MCP servers, agents and skills waits for your approval on each
  machine before it is written.
- **Live agents feed.** Sharing your running agents with your other machines is off for new
  installs. When on, it sends project, agent, status, tool and session id every 15 seconds;
  prompt and reply text only with a second opt-in, with secret-looking text redacted.
- **Deleting.** Settings > Sync has "Delete synced data" and "Delete account". Sign out of sync
  on your other machines first, or they upload their copy again.

[docs/second-machine.md](docs/second-machine.md) walks through signing in, the recovery key, the
approvals and the live feed on a second machine. `PRIVACY.md` lists everything sync uploads and,
under "Uninstalling", every file and keychain entry Mnemos leaves on a machine; `SECURITY.md`
says how to report a vulnerability.

## FAQ

- **Is this affiliated with the other projects named Mnemos?** No. Mnemos IDE is not affiliated
  with mnemos-dev/mnemos, s60yucca/mnemos or Ori-Mnemos.
- **Is it affiliated with Anthropic or Warp?** No. Not affiliated with or endorsed by Anthropic or
  Warp.
- **Linux?** Untested, not unsupported. Builds may work; issues and fixes are welcome.
- **Can I use it at work?** Running an unmodified build triggers no AGPL obligations. If you
  distribute a modified build, you must offer its source.
- **Why do shells inside Mnemos have `WARP_TERMINAL` set?** It mirrors `MNEMOS_TERMINAL` as a
  compatibility shim, so upstream Warp agent plugins (the Gemini CLI and OpenCode extensions)
  still recognise the terminal.
- **Does `mnemos login` work?** No. Warp's command-line device-code sign-in is not supported;
  sign in to sync from the app.

## License

Mnemos IDE is licensed under the **GNU Affero General Public License, version 3 only**
(`LICENSE-AGPL`), like the Warp code it is based on. The UI framework crates
`crates/mnemos_ui_core` and `crates/mnemos_ui` are under the **MIT license** (`LICENSE-MIT`), as
upstream, and `ide/claude-plugin` is also offered under MIT. `NOTICE` has the attribution to Warp
and the details. The sync server is not part of this repository and its source is not public.

## More

- [CONTRIBUTING.md](CONTRIBUTING.md): building, testing and sending changes
- [SECURITY.md](SECURITY.md): reporting a vulnerability privately
- [PRIVACY.md](PRIVACY.md): what stays local, what sync uploads, and how to uninstall
- [PACKAGING.md](PACKAGING.md): updates, verifying downloads, packaging for a distribution
- [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)
- [NOTICE](NOTICE): attribution and licensing
- [docs/second-machine.md](docs/second-machine.md): signing in and setting up sync on a second machine
- [docs/upstream-diff.md](docs/upstream-diff.md): what was removed or disconnected from Warp
- [ARCHITECTURE.md](ARCHITECTURE.md): how the code fits together
- [AGENTS.md](AGENTS.md): detailed contributor notes, also read by coding agents
- [RELEASING.md](RELEASING.md): how releases are built and published

Copyright (C) 2026 TwinCurrent ApS. Based on Warp, Copyright (C) 2020-2026 Denver
Technologies, Inc.
