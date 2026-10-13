# Mnemos IDE (Warp fork)

The guide for contributors and for coding agents (Claude Code, Codex, Gemini CLI, OpenCode and others) working in this repo. `CLAUDE.md` imports this file; keep shared guidance here, not there.

## What this is

**Mnemos IDE** is a **fork of [Warp Terminal](https://github.com/warpdotdev/warp)** rebranded as Mnemos, with a layer of our own Rust crates (`mnemos-ide-*`) under `ide/`. The product is a GPU-rendered, block-based terminal with deep AI/agent integration. The shipped app keeps the name `Mnemos` (`Mnemos.app`, binary `mnemos`, bundle id, data dir, installer); "Mnemos IDE" is the product name in docs and descriptions. Until 2026-10-08 the crates were `tcb-*` under `tcb/` (TCB = TwinCurrent Build, the old Electron app, also called v1); see "Names that kept `tcb`" below.

- **Upstream:** Warp Terminal (Rust, GPUI-derivative, native renderer), cloned from `github.com/warpdotdev/warp` on 2026-04-29. The whole app was rebranded from Warp to Mnemos. If you add Warp as a git remote, disable pushing to it.
- **Downstream additions:** eight `ide/` crates (all workspace members) plus `ide/claude-plugin` (a Claude Code plugin, not a crate). They add multi-project tabs over a cross-machine project registry, the agents panel (running agents and past Claude sessions, both machines'), team sessions (a Claude session you allow starting others in panes next to it), Claude session restore across restarts, the git tree panel, and sync against the Mnemos sync server (`server_root_url` in `app/build.rs`).
- **Binary:** `mnemos` (renamed from prior `tcb` / `warp`). Single channel: `stable`. Bundle ID: `dev.mnemos.Mnemos`.
- **License:** AGPL-3.0-only (inherited from upstream), with the two UI framework crates under MIT; see `NOTICE`.
- **Platforms:** Windows 11 x64 and macOS arm64 are both daily-driven, and both are published through the one release manifest (`RELEASING.md`). Linux code paths exist upstream, but no Linux build is published or tested.
- **Contributor docs:** `README.md`, `CONTRIBUTING.md`, `SECURITY.md`, `PRIVACY.md`, `ARCHITECTURE.md`, `PACKAGING.md`, `docs/upstream-diff.md`.

## Repo layout

| Path | Purpose |
|------|---------|
| `app/` | The main binary. Native window per-OS (Cocoa/Metal on macOS, Win32 + wgpu on Windows, X11 + wgpu on Linux). 100+ internal modules including the `ai/` agent harness. Contains `app/channels/stable/` with icons + desktop entry. |
| `crates/` | ~55 reusable Mnemos/Warp crates (see `Cargo.toml` workspace members): core UI/terminal/editor stack (`mnemos_*`, `ui_components`) plus shared infra (`ai`, `vim`, `lsp`, `languages`, `settings`, `persistence`, `managed_secrets`, `http_client`, `websocket`, …). |
| `ide/` | Mnemos IDE extensions, the crates we own (see below). Named `mnemos-ide-*` because a bare `mnemos-core` would collide with upstream's `crates/mnemos_core` (Cargo treats `-` and `_` alike). |
| `script/` | Build automation. `script/run` for dev (it hands off to `script/macos/run` on macOS). `script/bundle` hands off to `script/macos/bundle` or `script/windows/bundle.ps1`. Releases: `script/macos/release` and `script/windows/release.ps1` (see `RELEASING.md`). `script/ide-check` runs the mnemos-ide crates' tests and clippy, the leak check and the cargo-deny gates. `.github/workflows/release.yml` runs the two release scripts from a signed version tag; `pr.yml` and `dco.yml` check pull requests. |
| `resources/` | App assets (icons, themes). |
| `.cargo/`, `.clippy.toml`, `.rustfmt.toml`, `rust-toolchain.toml` | Cargo / lints / pinned toolchain. |
| `.twincurrent-build/` | Gitignored per-checkout directory (the name predates Mnemos IDE). The release scripts read `.twincurrent-build/.env` for their credentials; nothing in the app reads it, and it does not sync. |
| `Cargo.toml` | Workspace root (resolver v2; members are `crates/*`, `app` and the 8 `ide/*` crates). Release bundles use the thin-LTO `release-lto` profile, which sets `debug = false`; only `release-lto-debug_assertions` (`rltoda`) keeps DWARF for Sentry. |
| `Cargo.lock` | About 390 KB, a large dep graph. The release scripts build with `--locked`. |
| `ARCHITECTURE.md` | In-tree architecture doc (was `TCB_ARCHITECTURE.md`): how the code is put together today. |

## The Mnemos IDE layer (`ide/`: 8 crates + a Claude Code plugin)

These are the only crates we own outright. Everything else is upstream Warp/Mnemos.

| Crate | Role |
|-------|------|
| `mnemos-ide-core` | Shared types (`Project` in `types.rs`), paths (`paths.rs`: the data dir and its `MNEMOS_DATA_DIR` override, legacy `TCB_DATA_DIR` still read; `env_var_os`/`env_var` read a renamed `MNEMOS_*` variable with its `TCB_*` fallback), error types, and the cross-machine registry merge (`registry.rs`). No I/O beyond creating the data dir. Used by the app and by `mnemos-ide-projects`, `mnemos-ide-sync`, `mnemos-ide-sessions` and `mnemos-ide-agent-restore`; the other mnemos-ide crates do not depend on it. |
| `mnemos-ide-agents` | Tools-panel list of running agent sessions (Claude Code, Codex, Gemini and OpenCode), grouped by project, and below them SESSIONS: the past Claude Code sessions of every project with a checkout here, both machines' once transcript sync has pulled them, wired in `app/src/workspace/view/ide_sessions.rs` (ARCHITECTURE.md "Claude sessions"). A session click resumes the original, right-click a fork (`--fork-session`), always as a terminal tab inside the project's existing tab (any window), never a new project tab; a session running here switches to it, one running on the other machine is never resumed in place (fork or `open` only), and neither is one whose newest turns were pulled from another machine in the last 10 minutes, counted from the pull (`SessionState::PulledRecently`: both clicks fork, since that machine may still run it unseen; a click re-checks the transcript and the live lists first, so a row a scan old cannot resume in place). Clicking an agent row switches to its tab; cancel exists only for Claude (Ctrl-C into the PTY, `AgentKind::supports_cancel`). Source: `app::terminal::cli_agent_sessions::CLIAgentSessionsModel`. The live feed (`agents-live:<machine>`, `app/src/workspace/live_agents.rs`) publishes only while `sync.live_feed.enabled` is on (off by default; an install already signed in when the setting arrived keeps it on through a private pin), status only unless `sync.live_feed.share_prompt_text` is on, secrets redacted either way; it reads the other machines' keys only while an Agents panel is on screen, and deletes its own key when turned off and on sign-out. Other machines' sessions get an `open` pill that opens their claude.ai Remote Control URL in the system browser (`Workspace::open_remote_session`, through `ctx.open_url`). The URL is checked with `is_remote_control_url` in the left panel and again in the workspace; a turn that ended with background work still running stays in WORKING as "N background tasks". |
| `mnemos-ide-projects` | `Project` model, its on-disk store, and the `ProjectTabs` state (open projects + active id) the workspace keeps beside its native tabs. Every native workspace tab *is* a project. Since 2026-09-24 the registry is cross-machine: records carry `paths.{darwin,win32,linux}` + an `open` flag (the tab set on every machine), `resolve.rs` finds a record's local checkout by git remote or name, and `reconcile.rs` plans which tabs to link/open/close. Closing a tab hides the project (`open=false`), never deletes it. Never use `project.path` in app code; use `Project::local_path()`. See ARCHITECTURE.md "Cross-machine projects". |
| `mnemos-ide-sync` | Rust port of v1 `admin-auth` + `sync-client` + `sync-manager`: sign-in + state sync against the Mnemos sync server (`server_root_url`; today only the one the makers run, self-hosting is planned), `notify` file watching. `projects` merges as a union (`mnemos_ide_core::registry::merge`) on push and pull, polled every 30 s; `workspace_state.json` is local-only. Claude memory dirs use Claude Code's slug rule. Desktop sign-in is PKCE (`desktop_auth.rs`: a one-time code in the `mnemos://` redirect, traded with the verifier at `POST /api/auth/desktop-exchange`; the server keeps sending pre-PKCE clients tokens in the redirect until 2027-01-31). Every request carries `X-Mnemos-Sync-Protocol` (`protocol.rs`); a 426 stops sync as `UpdateRequired`. `transcripts.rs` syncs Claude Code session transcripts through the server's blob store (`/api/tcb/blobs`), rewriting `cwd` per machine so `claude --resume` works on both. Sessions sync wherever Claude was started: a project, else a code folder (on a machine without that folder, home-relative if it was under home, else unchanged), else home-relative (mapped onto the other machine's home), else the absolute cwd kept unchanged; never from inside `~/.claude`, the Mnemos data dir or a system temp folder (`/tmp`, `/var/folders`, `%LOCALAPPDATA%\Temp`, `is_temp_cwd`), and never from a meta with a `..` path. Pulled transcripts are marked so the memory checkout scan never stats their cwd. Executable content (hooks, Claude Code settings commands, MCP servers) goes through `gates.rs`: hooks sync is opt-in per machine, and incoming changes wait for approval by content hash. `settings.toml` syncs per key (`settings_toml.rs`). Signed-in account required. Per-project sync opt-out (`syncDisabledProjectIds`) covers transcripts too, both ways. One machine id (`machine.rs`, `<store>/machine_id`) names transcript segments and rides in the live feed as optional `machine_id`. Drive teams are joined by choice per machine (`team_choice.rs`, `<store>/team_choice.json`); a machine that synced under an earlier build keeps its first listed team. |
| `mnemos-ide-git-tree` | The git panel in the code-review header: branches, remotes, tags and the commit graph, with checkout. Replaced Warp's `DiffSelector`. `model.rs` wraps `git2`; `lane_router.rs` assigns graph lanes; `view.rs` renders. See "The git tree panel" below. |
| `mnemos-ide-sessions` | Reads Claude Code's `~/.claude/projects/<slug>/<uuid>.jsonl` transcripts for the agents panel's SESSIONS: a project root's sessions, subfolders included (`list_for_root`), each labelled from a bounded head of the file. Slugs use `mnemos-ide-core`'s rule, the one transcript sync files pulled sessions under. No UI, storage, sync or network of its own. |
| `mnemos-ide-agent-restore` | Carries live `claude` sessions across an app restart (an autoupdate, for one): stores pane uuid -> Claude session id (+ cwd, + whether it ran with permissions bypassed, which the resume then passes as `--dangerously-skip-permissions` again) in the local-only `store/agent_sessions.json` and resumes each in the same pane on the next launch, as a fork (`--fork-session`) instead when another machine may still be running it (`guard::resume_mode`: listed in `agents-live`, or newest turns pulled from elsewhere after this machine last had it open, by the pull time `PulledOrigin::at`), so it never takes over that machine's Remote Control session. Signed in, it first waits (up to 10 s) for the first live-agents answer and the first transcript pull (queued before the initial push and run ahead of the startup sweep); signed out, not at all. Each pane then gets its command once its shell is up, never parked during startup (up to 60 s). A pane the user typed or ran something in meanwhile, or whose shell never came up, gets no command: its entry is carried into the next snapshot (`queue::defer`) and the session stays in the sessions list until resumed in place from there. Consume-on-read, validated and capped, so a bad restore cannot loop. |
| `mnemos-ide-team` | The UI-free half of team sessions: a coordinator Claude Code session starting worker sessions in panes next to it. Name/model/branch/brief validation (`names.rs`), the `claude --name ... "$MNEMOS_TEAM_BRIEF"` command line with the brief kept out of the shell's parser (`command.rs`), spawn caps of 6 live and 3 a minute (`limits.rs`), leases on cpu, chrome and ports (`leases.rs`), git worktrees that are only removed when clean and merged (`worktree.rs`, records in `<git common dir>/mnemos-team/worktrees.json`), the team board reader (`board.rs`) and the MCP JSON-RPC messages and tool schemas (`mcp.rs`). The app half is `app/src/workspace/team_control.rs` (a loopback MCP server, only with `agents.team.enabled`; the bearer token is the pane's `MNEMOS_CLI_AGENT_TOKEN`; Origin and foreign Host refused; first spawn per pane asks the user; spawned panes cannot spawn; bypass only with the local `agents.team.spawn_skip_permissions`; `agents.team.keep_awake` holds a no-idle-sleep assertion while an agent works) and `app/src/workspace/view/ide_team.rs` (split, title, focus, close in the caller's tab). The `mnemos-team` plugin connects Claude Code to it. See ARCHITECTURE.md "Team sessions". |
| `claude-plugin/` | Not a crate: a Claude Code plugin marketplace whose `mnemos-agents` plugin has hooks that stream session events to the agents panel. It is the plugin Mnemos installs and tells users to install: `bash script/install-claude-plugin` registers this directory as the `mnemos` marketplace and installs `mnemos-agents@mnemos`; the app's own install action (`plugin_manager/claude.rs`) adds the same `mnemos` marketplace from the public source repository, whose root `.claude-plugin/marketplace.json` points at `ide/claude-plugin/plugins/mnemos-agents`. Upstream's `warp@claude-code-warp` is no longer installed, only still recognised where it already is. Claude Code runs a versioned copy from `~/.claude/plugins/cache/mnemos/mnemos-agents/<version>/`, not the repo, and `~/.claude/plugins` does not sync: after changing the plugin, bump `version` in `plugins/mnemos-agents/.claude-plugin/plugin.json`, `.claude-plugin/marketplace.json` and the repo root's `.claude-plugin/marketplace.json` (a test in `claude_tests.rs` checks they agree), then on each machine run `claude plugin marketplace update mnemos && claude plugin update mnemos-agents@mnemos` and restart Claude sessions. The app's own update action runs the same two commands (`ClaudeCodePluginManager::update`); it never removes the marketplace, and adds one from the source URL only when no `mnemos` marketplace is registered. From 1.2.0 the plugin sends the terminal's `MNEMOS_CLI_AGENT_TOKEN` with each event; bare events from an older installed plugin are still accepted while the pane runs `claude`, until it is updated (ARCHITECTURE.md "CLI agent events"). The `mnemos` marketplace is registered as a directory source by absolute path, so when the checkout or the plugin directory inside it moves (the plugin moved from `tcb/claude-plugin` to `ide/claude-plugin` on 2026-10-08; a machine registered before that has a dangling path, and `marketplace update` fails), re-point it by hand (the app does not): `claude plugin marketplace remove mnemos && claude plugin marketplace add <repo>/ide/claude-plugin && claude plugin install mnemos-agents@mnemos`, then `claude plugin marketplace update mnemos` to verify. Every hook sends `remote_url` once the transcript has one; the stop hook also sends `background_tasks`. From 1.3.0 the plugin also carries team rules and a status board for several sessions on one repository (`scripts/team.sh`, files in `<git common dir>/mnemos-team/`, shared by every worktree, never committed): `on-team-context.sh` adds `rules.md` and the board to a session's next turn only when they changed since that session last saw them (all of it again on SessionStart), and stays silent where neither file exists; tested by `tests/team-context.sh`. |

Notes:
- `tcb-browser` became `mnemos-ide-browser` and was removed in 2026-10 along with the browser pane; the other planned `tcb-*` crates were never built.
- Licensing (see `NOTICE`): everything is AGPL-3.0-only except the UI framework crates `crates/mnemos_ui_core` and `crates/mnemos_ui` (MIT, as upstream). The `ide/mnemos-ide-*` crates are AGPL-3.0-only; `ide/claude-plugin` is TwinCurrent's own code and is also offered under MIT (its `plugin.json` and `LICENSE`). Our edits inside upstream files are marked `Mnemos IDE:` (or `Mnemos IDE cut`) in comments, formerly `TCB:`; keep the marker on new edits so upstream merges can find them.

### Names that kept `tcb`

The 2026-10-08 rename (`tcb-*` -> `mnemos-ide-*`, `Tcb*` -> `Ide*`, `tcb_*` -> `ide_*`) left these alone on purpose, because installed apps, the server or other machines depend on them:
- **Wire names, unchanged:** the sync API under `/api/tcb/*` (`SYNC_API` in `ide/mnemos-ide-sync/src/client.rs`; the server is a separate repo and older clients still call it), every sync state key (`projects`, `claude-memory:*`, `agents-live:*`, `claude-session-index`, ...), the skipped `twincurrent-build:*` keys and the `claude-sessions/` blob prefix.
- **On disk:** the admin-login keyring services `tcbv2` / `tcbv2-<profile>` / `tcbv2-isolated` (see "Don't"), the `Mnemos` data dir and its `store/` files, the legacy seed dirs `TwinCurrent Build/` and `twincurrent-build-ui/`, the SQLite column `tabs.tcb_project_id` and its migration (never rename an applied diesel migration), `.twincurrent-build/`, the transcript temp suffix `.tcb-sync.tmp` and the pull backup dir `~/.claude/backups/tcb-sync-<date>/`.
- **Read as a fallback:** the env var `TCB_DATA_DIR`, read when `MNEMOS_DATA_DIR` is unset or empty.
- **History and data:** "TCB v1" (the Electron app) and the original private repo name `TCB` (registry `gitRepo` values and `claude-memory:<pid>:<owner>/<repo>` keys derive from the repo name, so renaming a repo needs its own key migration).

## Architecture stack (top → bottom)

```
   ┌────────────────────────────────────────────────────────┐
   │  app/  - binary entry, root_view, OS window glue       │
   │  • app/src/lib.rs::mnemos::run()                       │
   │  • app/src/bin/mnemos.rs sets Channel::Stable          │
   │  • Sidebar = Mnemos IDE project store                  │
   ├────────────────────────────────────────────────────────┤
   │  UI    mnemos_ui  →  mnemos_ui_core (GPUI-derivative)  │
   │        ui_components  +  mnemos_editor                 │
   │        Renderer: Metal (mac) | wgpu (Win/Linux)        │
   ├────────────────────────────────────────────────────────┤
   │  Terminal layer                                        │
   │    mnemos_terminal - VT/ANSI parser, grid, shell intel │
   │    local_tty (app/src/terminal): PTY, openpty/ConPTY   │
   │    input_classifier (ONNX/fasttext) for cmd detection  │
   ├────────────────────────────────────────────────────────┤
   │  Editor stack: mnemos_editor + vim + lsp + languages   │
   │    32 tree-sitter grammars                             │
   │    Full vim modes, operators, text objects             │
   │    LSP servers spawned via async-process               │
   ├────────────────────────────────────────────────────────┤
   │  AI / agents                                           │
   │    ai (built-in agent client; needs Warp's server,     │
   │       off by default: `builtin_agent` feature)         │
   │    mnemos-ide-agents (UI panel listing sessions)       │
   ├────────────────────────────────────────────────────────┤
   │  Networking                                            │
   │    mnemos_server_client (auth, drive, cloud_object)    │
   │    mnemos_graphql + Cynic + graphql-ws-client          │
   │    websocket (tungstenite native / ws_stream_wasm)     │
   │    http_client (reqwest + OAuth2)                      │
   │    remote_server (SSH + protobuf for remote dev)       │
   │    ipc (Unix sockets / Win named pipes)                │
   ├────────────────────────────────────────────────────────┤
   │  Storage / settings                                    │
   │    settings (TOML public, OS-native private)           │
   │    persistence (Diesel + SQLite, ~136 migrations)      │
   │    managed_secrets (upstream cloud-agent secrets, GCP) │
   │    secure_storage (keychain / DPAPI: API keys, MCP)    │
   │    mnemos-ide-sync (projects, settings, ~/.claude)     │
   │      admin token in the OS keychain (keyring crate)    │
   └────────────────────────────────────────────────────────┘
```

## How to build & run

### Windows 11 x64

`script/run` and `script/bundle` are bash scripts; run them from Git Bash. `script/bundle` hands off to `script/windows/bundle.ps1`.

```sh
# Dev build
./script/run

# Release bundle for the stable channel
./script/bundle -CHANNEL stable -ARCH x64
```

```powershell
# Windows installer pipeline (under the hood)
./script/windows/bundle.ps1                  # compile binary
./script/windows/prepare_bundled_resources.ps1
# Inno Setup → script/windows/Output/MnemosSetup.exe
```

### macOS arm64

Xcode 26 moved the Metal compiler into a separately installed component (MetalToolchain). When that component is missing or stale, plain `xcrun metal` fails. The `mnemos_ui` build script (`crates/mnemos_ui/build.rs`) then runs `script/macos/metal_toolchain` itself, which mounts the locally cached toolchain read-only and writes an `xcrun` shim to `target/metal-shim/`; the build compiles the shaders through that shim and says so in a cargo warning. `script/macos/run`, `script/macos/bundle` and `script/ide-check` also put the shim on PATH before building. Nothing needs exporting. The build still panics on a genuine shader error, and when there is no cached toolchain to mount (`script/macos/metal_toolchain` then prints how to install one).

```sh
./script/run                # dev build: bundles target/debug/bundle/osx/Mnemos.app and runs it
./script/run --dont-open    # build the bundle, do not launch it
cargo test -p mnemos-ide-core -p mnemos-ide-projects -p mnemos-ide-sync   # mnemos-ide crates
cargo check -p mnemos --features gui --lib --tests   # type-check the app crate and its tests
```

- The shim's mount lives in your per-user temp dir (`getconf DARWIN_USER_TEMP_DIR`), and the script only trusts a volume that you mounted; after a reboot the shim re-mounts it on its own, and hands the call to the real `xcrun` if it cannot. The permanent fix is `xcodebuild -downloadComponent MetalToolchain`, which needs `gdmf.apple.com` to be reachable.
- Releases: `script/macos/release` on the Mac, `script/windows/release.ps1` on Windows, as described in `RELEASING.md`.

### Both platforms

- `./script/run` runs on its own data profile: it sets `MNEMOS_DATA_PROFILE=dev` unless you set it yourself (`MNEMOS_DATA_PROFILE= ./script/run` opts out). When a profile is set and `MNEMOS_DATA_DIR` is not, it also points `MNEMOS_DATA_DIR` at a per-profile Mnemos IDE store: `~/Library/Application Support/Mnemos-<profile>` on macOS, but only for builds with debug assertions (the default dev build; not `--release` or a release `--profile` such as `release-lto` or `rlto`), and `%APPDATA%\Mnemos-<profile>` on Windows (Git Bash) in every build. On Linux it sets none (a debug build sets it itself, see the next bullet). The Mnemos IDE store has to move exactly when the app's SQLite workspace snapshot moves, because its pane uuids only mean something against that snapshot; a release-like macOS `.app` keeps the installed app's snapshot, so it keeps the installed app's Mnemos IDE store too. Opting out of the profile drops the `MNEMOS_DATA_DIR` default as well.
- **`MNEMOS_DATA_PROFILE` alone does not isolate a build from the installed app.** In builds with debug assertions, `MNEMOS_DATA_PROFILE=<name>` moves upstream's state dir, secure-storage domain and `~/.mnemos-<name>` home config (`ChannelState::data_profile` in `crates/mnemos_core/src/channel/state.rs`); release-like builds ignore it there. In every build, release included, it switches the mnemos-ide-sync keyring entry to `tcbv2-<name>` (`mnemos_ide_core::paths::data_profile`, read by `mnemos_ide_sync::auth::keyring_service`). On macOS it moves `config_local_dir` (and with it `settings.toml`) to `~/.mnemos-<name>` too. It does not move the Mnemos IDE store by itself. Without `MNEMOS_DATA_DIR`, a build reads and writes the live `<data dir>/Mnemos/store/` (`projects.json`, `workspace_state.json`, `agent_sessions.json`), and the installed app reconciles its tabs from that `projects.json` every 2 s. The legacy `TCB_DATA_DIR` still works wherever `MNEMOS_DATA_DIR` does (the new name wins when both are set and non-empty), and `script/run` maps it onto `MNEMOS_DATA_DIR`. `./script/run` sets both variables for the default dev build (bullet above). A debug build that is not a release bundle also defaults them itself, so plain `cargo run` is isolated too: `app/src/bin/mnemos.rs` calls `mnemos_ide_core::paths::apply_dev_isolation_defaults` first thing, which sets `MNEMOS_DATA_PROFILE=dev` when it is unset and, when no data dir is set, `MNEMOS_DATA_DIR=<data dir>/Mnemos-<profile>` on every platform. `MNEMOS_DATA_PROFILE=` (empty) opts out of both. Release-like builds and `release_bundle` builds never default anything.
- **Tests stay out of your real config.** With `mnemos_core`'s `test-util` feature (dev-dependencies and the integration harness), while no binary has set a channel, `config_local_dir()` (`settings.toml`) and `data_dir()` point into a per-process temp dir (`ChannelState::unit_test_home_dir`, `mnemos-unit-test-home-<pid>`). `mnemos_home_config_dir` (skills, `.mcp.json`) still points at the real home.
- `script/ide-check` runs `cargo test` and `cargo clippy --all-targets -- -D warnings` over every `ide/mnemos-ide-*` crate, the leak check (`script/oss/leak-check.sh`) and `cargo deny` (licenses, sources and bans offline; advisories against the fetched RustSec database, skipped with `MNEMOS_SKIP_ADVISORIES=1`), and fails if any step fails. CI runs it on every pull request (`.github/workflows/pr.yml`, with a scratch `HOME`; upstream's `script/presubmit` cannot pass here); run it before cutting a release too. The test that parses your real `~/.claude/projects` (`mnemos-ide-sessions`) only runs with `MNEMOS_TEST_REAL_CLAUDE=1`; the script's header shows how to run everything against a scratch `HOME`. The cheap text checks (`script/check-namespaces` plus the billing-copy check, allowlist `script/oss/grep-checks-allowlist.txt`) run through `script/oss/grep-checks.sh`, which `script/ide-check`, `script/presubmit` and CI all call; `.github/workflows/dco.yml` checks the DCO sign-off of every PR commit (`script/oss/check-dco.sh`).

- **Only official releases update themselves.** A `release_bundle` build gets an updater only with `MNEMOS_OFFICIAL_RELEASE=1` (set by the release scripts) or an explicit `RELEASES_BASE_URL`, and then only together with the update signing public keys from `script/update-signing.pub` (or `MNEMOS_UPDATE_PUBLIC_KEYS`); `app/build.rs` refuses to build an updater without one. The updater fetches the static `channel_versions.json` anonymously, verifies its Ed25519-signed per-platform payload and checks each download's sha256 before installing (`crates/channel_versions/src/signed.rs`, `app/src/autoupdate/`). `MNEMOS_UPDATER=off` turns it off at build or run time. The signing tool is `update_signing` in `crates/channel_versions`.

Profiles: `dev` (default, debug assertions); `release-lto` (release + thin LTO, used by `script/macos/bundle`) and its short alias `rlto` (used by `script/windows/bundle.ps1`, where long paths break the build); `release-lto-debug_assertions` and its alias `rltoda` (the same plus debug assertions, for local/dev channel bundles).

`script/compile_icon` reads from `channels/stable/icon/`: the `channels/stable/` dir name **must not be renamed** (recent commit fixed exactly this regression).

Build logs go in the gitignored `.build-logs/`.

## Channels

Only **stable** ships (the `local`/`dev`/`preview`/`oss` binaries were removed, and `Channel::Oss` is gone). The channel of a build that has not set one, unit tests included, is `Local` (app id `MnemosLocal`). Channel-conditional code hardcodes stable; the bundle script still accepts `-CHANNEL` for future-proofing. Binary `mnemos`/`mnemos.exe`, bundle ID `dev.mnemos.Mnemos`.

## The git tree panel (`ide/mnemos-ide-git-tree`)

Lives in the code-review header, where Warp's `DiffSelector` used to be
(`app/src/code_review/code_review_header/header_revamp.rs::render_diff_mode_dropdown`).
The app owns the `ViewHandle<GitTreeView>`, pushes data in via
`code_review_view.rs::update_diff_selector_selection`, and handles emitted events in
`handle_git_tree_event`.

**It's a dropdown, not an inline panel.** `render()` returns a compact trigger button showing
where HEAD is; the tree opens as a positioned overlay (`Stack::add_positioned_overlay_child`).
Rendered inline it swallowed the diff header. The panel needs its own opaque background: it
floats over the file list, and a translucent surface lets the diff show through the commit text.

**One flat row list, not nested widgets.** `UniformList` requires a uniform row height, so
sections, refs, pull requests and commits all render at `ROW_HEIGHT` (24). `rebuild_rows()` flattens
sections + visible children into `Vec<TreeRow>`. Don't reach for nested scrollables: a plain
column would re-render up to `MAX_COMMITS` (1000) rows every frame. **Commit rows are one line**
for the same reason: a two-line commit row makes *every* row that tall, which is what made the
first version look so sparse.

**ASCII glyphs only.** The UI font has no coverage for `▾ ▸ ● ⑂ ⌾`; on Windows they rendered as
`*` and `ᵛ`. Use `v` / `>` / `*`, or a real icon element. Verify anything non-ASCII on Windows
before shipping it.

**Interaction, deliberately asymmetric:**

| Target | Click | Right-click |
| --- | --- | --- |
| Section header | collapse / expand | - |
| Branch | **check it out** | - |
| Tag | check out (detaches HEAD) | - |
| Pull request | check out its head branch (fetches `origin/<head>` first if missing; fork PRs refuse with a banner) | - |
| PR's `open` chip | open the PR on GitHub in the system browser | - |
| Current branch | select its tip commit | - |
| Commit | select → load diff | check out (detaches HEAD) |
| Selected commit's chips | `checkout` / `branch` | - |

Commits need *two* clicks to check out (select, then chip) so a stray click can never detach
HEAD. Branches check out on first click because that is unambiguous.

**Checkout is always `CheckoutBuilder::safe()`**: it refuses rather than discarding uncommitted
work. That refusal is a normal outcome, not a bug: it surfaces in the view's status banner via
`set_status`, and `run_git_tree_checkout` re-syncs the tree either way. Never switch this to
`force()`.

Empty sections are omitted entirely rather than rendered as dead headers. `RefKind::RemoteBranch`
checkout creates a local tracking branch first (`origin/foo` → `foo`), matching VS Code; note the
`split_once('/')` in `GitGraphModel::checkout_branch`, so slashes inside branch names survive.

**Pull requests** (`prs.rs`) come from `gh pr list --json PR_LIST_FIELDS -R <owner/repo>`, which the
app runs (`util/git.rs::list_open_prs`) when the panel opens, and on the tree's normal refresh while
it is open, at most once per 60 s per repo (`PR_LIST_REFRESH_INTERVAL`, failures included). `-R`
always comes from `origin` via git2 (`origin_github_slug`): bare `gh` in this repo resolves to
`upstream` and would list Warp's PRs. A non-github.com origin, a missing or signed-out gh, or no
network all just omit the section; only the first failure is logged at info. Parsing, the CI rollup
(CheckRun and StatusContext; any failure -> `ci fail`, else anything unfinished -> `ci pending`,
else `ci pass`; no checks -> no chip) and the click decision (`pr_checkout_plan`) are pure and
unit-tested. The section sits right after BRANCHES. A PR click reuses the branch checkout path, so it
is still `CheckoutBuilder::safe()`. A head branch that has not been fetched yet goes into
`refs/remotes/origin/*` first (`run_git_tree_fetch_checkout`, a `+refs/heads/x:refs/remotes/origin/x`
refspec that never touches the working tree). Cross-repository (fork) PRs are never checked out. The
PR row uses `with_defer_events_to_children()` so its `open` chip does not also trigger the checkout,
and `open` URLs are checked again with `is_github_pr_url` before `ctx.open_url`.

Related: `CodeReviewAction::SetAllFilesExpanded(bool)` powers "Collapse/Expand all files" in the
header menu (`file_expansion_menu_items`). It writes through to `RepositoryState::file_expanded`
so `should_auto_expand_file` doesn't silently re-expand on the next diff refresh.

## Working in this repo: orientation

- **If you're touching the terminal grid / ANSI / shell detection** → `crates/mnemos_terminal/src/shell/`. Bash version detection: `BASH_INPUT_REPORTING_MINIMUM_VERSION = 4.0`. Zsh extended-history stripping lives here.
- **If you're touching the PTY itself** → `app/src/terminal/local_tty/` for spawning (`unix.rs`: `openpty` plus a `Command` with a `pre_exec` hook; `windows/conpty_api.rs`: ConPTY), upstream `crates/mnemos_terminal` for state.
- **If you're touching the UI** → `crates/mnemos_ui_core` (layout/paint primitives), `crates/mnemos_ui` (platform), `crates/ui_components` (widgets), `crates/editor` (the `mnemos_editor` text editor). Pattern: immediate-mode render with retained component state.
- **If you're touching settings** → `crates/settings` for the dual-backend logic; settings.toml is synced per key by `mnemos-ide-sync` (key `mnemos-user-settings-v2`, `ide/mnemos-ide-sync/src/settings_toml.rs`): only settings marked `SyncToCloud::Globally` / `PerPlatform` in the registry (`app/src/settings/ide_sync_policy.rs`), minus the deny list in `settings_toml::DENIED_SETTINGS`. A new setting that names a program, a local path or widens agent execution must be `SyncToCloud::Never`. The old whole-file key `mnemos-user-settings` is neither pushed nor applied any more. Secrets never go in settings; see the keychain rule under "Don't".
- **If you're touching themes** → the built-in themes are Mnemos's own, not Warp's: Mnemos Night (default), Mnemos Day (default light), Mnemos Contrast and Mnemos Moss, all from the app icon palette (#F4F1EA / #C6F35A / #1B1033), then five image themes, Stone (light), Slate, Mountain Night, Mountain Dawn (light) and Fjord, each a ground colour over a background image bundled in `app/assets/bundled/jpg/themes/` (embedded by `Assets` in `app/src/lib.rs` on every GUI build, never fetched; the standalone CLI leaves them out; credited in `NOTICE` and the `ATTRIBUTION.md` next to them, and the image shows through at the theme's image opacity), then twelve community themes built from their projects' published palettes (not from Warp's theme files): Catppuccin Latte and Mocha, Dracula, Gruvbox Dark and Light, Kanagawa, Nord, One Dark, Rosé Pine, Solarized Dark and Light, Tokyo Night (credited in `NOTICE`). They are defined in `crates/mnemos_core/src/ui/theme/mnemos_themes.rs` (`mnemos()`, `image()`, `community()`, `all()`; shared with the onboarding dev binary, whose theme slide offers only the four Mnemos themes; contrast floors pinned by `mnemos_themes_tests.rs`, where the image themes meet the Mnemos bar against their ground colour and `image_themes_are_embedded_and_readable_over_their_image` in `app/src/themes/theme_test.rs` checks the foreground over the composited image, and where the community themes keep their published colours except that ANSI black never equals the background (Gruvbox Dark/Light and One Dark move it one step off, documented next to their palettes) and only their derived selection highlights are retuned through `with_selection_opacities`) and registered in `MnemosThemeConfig::new()` (`app/src/themes/theme.rs`), which is the only list the theme chooser and onboarding read. The chooser sorts by `ThemeKind`'s derived order, so the image variants follow `MnemosMoss` in that fixed order and the community variants sit alphabetically after them. Dracula, Gruvbox and Solarized reuse upstream's legacy variants, which every older build already parses, so they are safe to pick across synced machines; the other community variants and the five image variants are new and carry the same caveat as the `mnemos_*` ones below. Warp's other built-ins are gone from that list, but their `ThemeKind` variants stay (`#[schemars(skip)]`) so old `settings.toml` values still parse: the settings-file derive ignores serde aliases, and a value that fails to parse inhibits writes to the key. A saved legacy value is never rewritten; `ThemeKind::resolved` renders it as Mnemos Day (light ones) or Mnemos Night, in `derived_theme_kind`, `MnemosThemeConfig::theme()` and the chooser, so a value hot-reloaded from sync also renders right. Do not add a launch-time migration that writes `mnemos_*` values until every machine sharing synced settings runs a build that parses them: an older build cannot parse the new variants, falls back to Warp's Dark, reports invalid settings and stops saving the theme key. The same holds for a theme the user picks, so upgrade all synced machines together. Delete the legacy variants only after that. `MnemosTheme::inferred_color_scheme` compares foreground against background luminance for every theme, custom and Base16 ones included (a gradient uses its midcolour), so a mid-grey foreground on white is dark on light; `theme_tests.rs` pins this. Status colours (`ui_error_color` and friends) and the text selection, context selection and workflow-parameter highlights derive from the theme (`color.rs`), and the default selection fill of `Text`/`FormattedTextElement` follows the theme through `set_default_selection_color` in `Appearance`; `mnemos_themes_tests.rs` pins their contrast. The onboarding theme and login slides draw their preview from the active theme (`onboarding::theme_picker_visual`), and the first-run overlay palette (`PhenomenonStyle`) is the icon's violet/off-white/lime.
- **If you're touching the database** → `crates/persistence/migrations/`. `diesel.toml` points to `crates/persistence/src/schema.rs` (regenerated via `schema.patch`).
- **If you're touching agent integration** → `app/src/terminal/cli_agent_sessions/` and `ide/mnemos-ide-agents/` for the agents panel (third-party CLI agents). The built-in agent has no client-side provider: it sends each request, with any BYO provider keys (`crates/ai/src/api_keys.rs`, kept in upstream `secure_storage`) and AWS Bedrock credentials, to the server's `/ai/multi-agent`. That client only builds with the non-default `builtin_agent` feature (`app/Cargo.toml`); without it agent requests fail locally and the API key settings are hidden. Passive prompt and code suggestions are likewise behind the non-default `passive_suggestions` feature. CLI agent events (OSC 777 `mnemos://cli-agent`) are trusted only with the terminal's `MNEMOS_CLI_AGENT_TOKEN`; bare ones need `agents.third_party.accept_untokened_cli_agent_events`, except Claude events from a `mnemos-agents` older than 1.2.0 (`cli_agent_sessions/event/mod.rs`).
- **If you're touching projects / sync** → the crates under `ide/`, wired into the app in `app/src/workspace/view/left_panel.rs`, `app/src/workspace/ide_sync.rs` and `app/src/auth/auth_manager.rs`.
- **If you're adding a language** → `crates/languages/` registry + drop a tree-sitter grammar. The `LanguageRegistry` lazy-loads.
- **If you're adding a vim feature** → `crates/vim/`. Modes: Normal/Insert/Visual/Replace. Default is **Insert** (matches bash/zsh vi mode).

## Don't

- **Don't push to Warp.** If you add `github.com/warpdotdev/warp` as a remote (conventionally `upstream`), set its push URL to something invalid (`git remote set-url --push upstream no_push`).
- **Don't run `gh` here without a repo pinned.** With a Warp remote present, gh may resolve to it, so a PR or issue command would target Warp. Pass `-R <owner>/<repo>`, or run `gh repo set-default <owner>/<repo>` once per clone.
- **Don't rename `app/channels/stable/`**: `script/compile_icon` is hardcoded to that path.
- **Don't add new OS keychain items.** Mnemos IDE code adds exactly one: the admin sync token (`ide/mnemos-ide-sync/src/auth.rs`, `keyring` crate, services `tcbv2` / `tcbv2-<profile>` / `tcbv2-isolated`, kept through the 2026-10-08 rename). Never rename them, not even with a copy-on-read migration: the admin server rotates the refresh token on every refresh, so a copied login forks the token and whichever build on the machine refreshes second (an older build, a rollback, a dev build from another checkout) is signed out. Upstream `crates/mnemos_ui_extras/src/secure_storage/` also keeps secrets (BYO provider API keys, MCP OAuth tokens, the sync recovery keys under `MnemosSyncRecoveryKeys`): the login keychain on macOS, DPAPI-encrypted files in the state dir on Windows. Store a new secret through one of those two. `managed_secrets` is upstream's server-side secret store for cloud agents (GraphQL, HPKE, GCP federation), not a local store.
- **Don't wire Firebase back in.** The `firebase` crate and the Firebase token exchange are gone; a Firebase token is refused locally (`fetch_auth_tokens` in `app/src/server/server_api/auth.rs`), and a browser redirect with neither a code nor sync tokens, or the CLI device-code login, fails at once (`AuthManager::refuse_firebase_sign_in`). `Credentials::Firebase` stays only as a persisted-user format. Cross-machine sync here is `mnemos-ide-sync` against the Mnemos sync server. Upstream's server calls (`ServerApi` in `app/src/server/server_api.rs`) go to `server_root_url` from the channel config (`app/src/bin/channel_config.rs`: the config `app/build.rs` embeds, or `mnemos-channel-config` when a non-bundle build finds it on PATH), which is the Mnemos sync server and has no upstream `/graphql`. Under the admin login (`Credentials::SessionCookie`), `ServerApi::send_graphql_request` bails before any network call, and `has_upstream_server_session` skips the cloud-object and workspace-metadata pollers and refreshes, the cloud-object update websocket (`cloud_objects/listener.rs`), and the refresh of the signed-in AI model list (`LLMPreferences::refresh_authed_models` in `app/src/ai/llms.rs`). Neither gate covers the rest: ServerApi's plain HTTP calls (the `/api/v1/` REST API, plus endpoints such as `/client/login`, `/ai/*` and `/current_time`) and the few GraphQL operations sent directly, such as `GetUser`, still go to `server_root_url` when they run. No update or changelog request goes out before the first window draws a frame (`AppContext::on_first_frame_drawn`, which chains callbacks; `AutoupdateState::register`, `ChangelogModel::new_deferred`). RudderStack telemetry needs both the non-default `telemetry` cargo feature and a `telemetry_config` (`ChannelState::is_telemetry_available`); no bundle has either, and no stable bundle builds `crash_reporting` (the macOS dev and preview channels still pull it in through `heap_usage_tracking`).
- **Don't drop the `cocoa_sentry` consideration** when touching macOS bundle features: a past fix removed it from default features and that needs to stay off (`f1785c1`).

## Status / phase

- **Mnemos rebrand:** complete (binary, channel dir, bundle ID).
- **Mnemos IDE layer:** every mnemos-ide crate is wired into the app. The unused `mnemos-ide-terminal` and `mnemos-ide-claude` crates were deleted in 2026-10. The Kanban + Roadmap board (`tcb-kanban`) was removed in 2026-10.
- **Sync is live** (`mnemos-ide-sync`, against the makers' sync server; an account there is needed for now, self-hosting is planned): `projects` (merged record by record), `settings`, the Mnemos `settings.toml` (per key, under `mnemos-user-settings-v2`), the `~/.claude` globals and directories listed in `GLOBAL_FILES` / `DIRECTORIES` (`ide/mnemos-ide-sync/src/manager.rs`; `~/.claude/.env` only where the machine turned it on in Settings > Sync, off by default, kept in the local `store/sync_options.json`), `claude-memory:*`, `claude-json-mcp-servers-v2` (`~/.claude.json` mcpServers without each server's `env` and `headers`; the old `claude-json-mcp-servers` key is never applied, and a push removes this platform's slot from it), and Claude transcripts through the server's blob store. Content that runs code is gated per machine (`ide/mnemos-ide-sync/src/gates.rs`, state in the local-only `store/sync_gates.json`): `claude-hooks`, `claude-statusline` and the command keys of Claude Code's settings sync only after the user turns on hooks sync in Settings > Sync, and every incoming hook, command set, MCP server entry or `mcp-servers/` file is written only after the user approves its content hash there. Optional end-to-end encryption (`crypto.rs`, `e2e.rs`): once turned on in Settings > Sync, every personal state value and transcript blob is sealed with XChaCha20-Poly1305 on the client and stored under `e2e:<key>` / `e2e/<path>`, next to a plaintext `e2e-meta` marker; the recovery key is shown once and kept per admin profile in upstream secure storage (`app/src/auth/ide_sync_e2e.rs`, item `MnemosSyncRecoveryKeys`), never in the `tcbv2` keyring entry. Team-scope state and `agents-live:*` stay plaintext. Plaintext a pre-encryption build writes to an encrypted account is merged into the encrypted copy and deleted by the next full pull (blobs: by the next transcript listing). `.twincurrent-build/` does not sync: nothing pushes it, and `apply_state` skips `twincurrent-build:*` keys. Settings > Sync can delete the account's synced data or the account on the server (staff accounts are refused; `GET /api/tcb/me` says which).
- **Panels act:** the agents panel switches tabs, cancels, and resumes past Claude sessions inside their project's tab. Claude runs in terminal tabs (the sessions list and agent restore resume sessions there), not through a streaming bridge.
- **Channel system:** simplified to stable-only.
- **Active dev surface:** both platforms. The platform-specific work since 2026-08 has mostly been macOS (Metal toolchain, Mac release and self-update).

## Reference docs in-tree

- `ARCHITECTURE.md` (was `TCB_ARCHITECTURE.md`): crates, cross-machine projects, sync, agent events, updates, and which upstream code is still compiled in.
- `RELEASING.md`: how each platform builds, uploads, signs and publishes a release, by hand or from `release.yml`, and the update signing key.
- `PACKAGING.md`: building Mnemos from source to ship it.
- `docs/upstream-diff.md`: what was removed, compiled out or disconnected from Warp, with commits.
- `README.md`: the front door (features, platforms, build from source, privacy).
- `PRIVACY.md`: what stays local, what sync uploads, and what the app sends to the server.
- `CONTRIBUTING.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md`, `NOTICE`.
- `CLAUDE.md`: imports this file, plus Claude Code specific notes.
