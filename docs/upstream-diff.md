# What Mnemos IDE removed or disconnected from Warp

Mnemos IDE is a fork of Warp. The upstream base is recorded in `script/oss/UPSTREAM_BASE`:
commit `d0f045c01bacbd845a631d07da30f277cfd2b98d` ("Auto oss vs cost efficient 50/50 A/B
test (#9355)", 2026-04-28) on `github.com/warpdotdev/warp`. This page lists what was removed or
disconnected relative to that commit, with the commits that did it, so a reader can check each
claim with `git show <sha>`.

Mnemos IDE is a hard fork: it does not merge Warp's main branch, and upstream fixes are ported
selectively, one change at a time. Edits inside upstream files carry a `Mnemos IDE:` (or
`Mnemos IDE cut`) comment so a port can find them.

It covers removals and disconnections only. What Mnemos added (the `ide/` crates, the agents and
sessions panels, sync, the git tree panel) is described in `AGENTS.md` and `ARCHITECTURE.md`.

## How to reproduce this list

In a clone that also has Warp's history (`git remote add upstream
https://github.com/warpdotdev/warp.git && git fetch upstream`):

```sh
BASE=$(grep -v '^#' script/oss/UPSTREAM_BASE)
git diff --shortstat "$BASE" HEAD                      # overall size of the fork
git diff --name-status -M "$BASE" HEAD | grep '^D'     # files deleted outright
git diff "$BASE" HEAD -- app/Cargo.toml                # features and dependencies
git log --diff-filter=D --format='%h %s' -- <path>     # the commit that deleted <path>
```

The private history starts with one root commit (`02d4fb5b5`, 2026-05-05) that already contains
the first month of changes, so a file removed in that window shows that root commit, not an
individual one. Everything after it has its own commit.

On `oss2/rc` at `ef0bade60`, `git diff --shortstat` against the base reports 3777 files changed,
102233 insertions and 169628 deletions; 900 files were deleted and 679 renamed (mostly the
`warp_*` to `mnemos_*` crate renames). The later cleanup commits below remove more.

## Removed

| What | Commit(s) | Notes |
| --- | --- | --- |
| `crates/app-installation-detection` and the local HTTP probe Warp's website called | `8aab78078` | The local server on 127.0.0.1:9277 now exists only in builds with the `jemalloc_pprof` feature, for heap profiling. |
| Firebase: `crates/firebase`, the token exchange against Google's securetoken endpoints and the `/proxy/token` fallback, anonymous users and the `SkipFirebaseAnonymousUser` flag | `3b8faf425` (API key), `6a97c7eee`, `924a4c335` | `firebase_auth_api_key` is empty in the baked config. A Firebase token is refused locally (`fetch_auth_tokens`), so stale Firebase credentials ask for sign-in again. A browser redirect that carries neither a code nor sync tokens, and the CLI device-code login (`authorize_device`), now fail at once with a clear message (`AuthManager::refuse_firebase_sign_in`). `Credentials::Firebase` and `FirebaseToken::Refresh` stay, because they are the persisted-user format in secure storage. |
| Voice input: `crates/voice_input`, the `/ai/transcribe` client, voice settings, mic buttons | `a515b23c6` | No audio is recorded or uploaded. `NSMicrophoneUsageDescription` stays so programs run in the terminal can ask for the microphone. |
| Computer use: `crates/computer_use`, its agent actions, permission UI and flags | `21db63f78` | The persisted permission and profile fields stay so saved profiles still deserialize; computer use is never enabled. |
| The wasm build: build scripts, `serve-wasm`, `crates/managed_secrets_wasm`, the web handoff and the wasm first-run dialog | `b98e45e9d` | `cfg(target_family = "wasm")` branches remain in upstream crates, but nothing builds them. |
| The plugin host, the `completions_v2` feature and `crates/command-signatures-v2` | `1e4d3d2b0` | |
| `crates/mnemos_js` and the `mnemos_completer` `v2` feature (JS completions over `rquickjs`) | `275b01ff9` | Unreachable once `completions_v2` was gone. The legacy completer branch of every `cfg(feature = "v2")` site is kept. |
| The bundled skills tree, its license rows and the `bundled_skills` feature | `539ea4518` | |
| The Windows antivirus scan, its Sentry tag and telemetry field | `e8092ec41` | |
| Billing and usage page, buy-credits banner, auto-reload, free tier and plan modals, pricing, upgrade links | `2670c9b72`, `217d99227` | The second commit replaced the leftover billing copy (the shared-session limit text, the AI fallback setting's description). |
| Server experiments and the local A/B layers | `2670c9b72`, `fc88ab330` | Each pinned to one arm: Tantivy palette search on, onboarding survey kept, standard auth token copy, no free tier default model override. Feature flags keep their Cargo defaults. |
| Share block, block permalinks, View Shared Blocks and its settings page, the block GraphQL client, shared block title generation | `d1df22276` | Replaced by a local "Copy as Markdown" (block context menu, cmd-shift-S, Blocks menu). |
| The referral theme status, its startup query and the referral reward modal | `4300bf615` | The `SentReferralReward` / `ReceivedReferralReward` `ThemeKind` variants stay so old settings parse. The hidden referrals settings page and its server calls are still compiled in. |
| Warp staging-server special cases (`StagingAccessBlocked`, `ChannelState::uses_staging_server`, checks for Warp's staging host) | `7b0869e68` | Warp's domain is now flagged by `script/check-namespaces`. |
| The Oss channel (`Channel::Oss`, the `mnemos-oss` binary name and app id) | `57b2fb933` | A source build of the stable binary has no telemetry, crash reporting or updater, so the channel had no purpose. The channel of a build that has not set one is now `Local` (app id `MnemosLocal`). |
| Compile-time `MNEMOS_USER_SECRET` login path | `14970f658` | |
| The `.warp` config symlink shim and the Linux Warp app-name fallbacks | `fd477d5c5` | |
| The Uber team special case in CLI agent detection | `c6a7bc701` | |
| OpenMnemos launch modal and the OpenCode debug actions | `a7ae08ccd`, `2670c9b72` | |
| Unattributed upstream art (Warp logo PNGs, marketing PNGs, background JPEGs of unknown provenance) and unused modal assets | `3a0a61a28`, `6f22b067f` | |
| Warp artwork and identity (dock tile icons, install image, logos, Warp screenshots in onboarding, Warp logo glyph in the bundled Roboto) | `42397c400`, `4cca06fec`, `2af89de8a`, `84c8177d6` | The dock tile plugin is built from source (`abe5c65a0`) instead of downloaded. |
| Warp's Linux package repositories (deb/rpm postinst added Warp's apt/yum repos) | `d5910ddb9` | |
| Bundled Anthropic SDK in the picker agent | `ae2b62b07` | Then the whole browser pane went (below). |
| Agent plugin installs from unregistered namespaces | `05d5779de` | Upstream's `warp@claude-code-warp` plugin is no longer installed; Mnemos installs its own `mnemos-agents`. |
| Warp's built-in themes from the chooser | `f5c015fb5` | Legacy `ThemeKind` variants stay so old `settings.toml` values parse; see `AGENTS.md`. |
| Channels other than stable (local, dev, preview, oss binaries) | `b5b2ade3f`, `40210a8e0` | `app/src/bin/{dev,local,oss,preview}.rs` deleted. |
| Warp's CI workflows, dependabot, issue templates | `fd9446ac3` | Mnemos's own are `.github/workflows/{pr,dco,release}.yml`. |
| Warp's docs, agent skills, `.warp` workflows, `specs/`, scratch dirs | `41ec6a6cd` | |
| Docker dev containers and dead scripts | `3a767e9ce` | |
| Upstream `docker://` URI handler | `809ef7279` | |
| Linear integration (`app/src/linear.rs`) and the Codex modal | `1a097782e` | |
| Oz launch modal and the OpenWarp launch modal | `2af89de8a`, root commit | The `oz_launch_modal` cargo feature is gone; the `oz` CLI link is now `mnemos`. |
| Warp's `DiffSelector` in the code-review header | `6fe734a5e` | Replaced by the git tree panel. |
| `default_adeberry_theme` cargo feature | root commit | |
| Free badge and "Compare plans" upsell on the Account page | `9d0b21b59` | |
| Warp-cloud settings-sync toggle and Sync settings page | `3db49cc95`, `002a98a0f` | `mnemos-ide-sync` owns config sync; its own Settings > Sync page replaced it. |
| `mnemos-ide-terminal` and `mnemos-ide-claude` (Mnemos's own unused crates) | `a3e6a56f0` | Listed because older docs mention them. |
| Kanban board (Mnemos's own, not upstream) | `592b27ebb`, `2c5ab9697` | Same. |
| Browser pane (Mnemos's own) | `2c25f2089`, `01dba23b3` | Same. |

## Compiled out by default

These are still in the source but only build with a non-default cargo feature that no bundle
script enables.

| What | Feature | Commit(s) | Without the feature |
| --- | --- | --- | --- |
| The built-in agent client (upstream's Oz agent), BYO provider keys and AWS Bedrock credentials | `builtin_agent` | `8b4331e13` | Agent requests fail locally instead of going to the server's `/ai/multi-agent`; keys are never attached to a request; the API key and Bedrock settings are hidden. Third-party CLI agents (Claude Code, Codex, Gemini, OpenCode) are unaffected. |
| Passive prompt and code suggestions | `passive_suggestions` | `5aa16c698` | The settings, palette toggles and enablement getters report them as off. |
| RudderStack telemetry | `telemetry` | `8f86e26a5` | See "Disconnected" below. |
| Sentry crash reporting | `crash_reporting` | `f1785c17c`, `d5fa4251e` | See "Disconnected" below. |

## Disconnected (compiled in, no destination)

| What | Where | Commit(s) |
| --- | --- | --- |
| Server endpoints | `app/build.rs` bakes a stable channel config: `server_root_url` `https://admin.twincurrent.dk` (upstream `https://app.warp.dev`), `rtc_server_url` `wss://admin.twincurrent.dk/rtc` (upstream `wss://rtc.app.warp.dev/graphql/v2`), `session_sharing_server_url` `wss://admin.twincurrent.dk/sessions` (upstream `wss://sessions.app.warp.dev`), `oz_root_url` `https://admin.twincurrent.dk` (upstream `https://oz.warp.dev`). A build that is not a `release_bundle` also honours `MNEMOS_SERVER_ROOT_URL` and the websocket overrides, for testing against a local server. | root commit, `16a38a4ca` |
| Update check | An anonymous GET of the static `{releases_base_url}/channel_versions.json`, acted on only when its Ed25519 signature verifies (`app/src/autoupdate/channel_versions.rs`, `crates/channel_versions/src/signed.rs`). Upstream asked `/client_version/daily` with the persistent install id (`X-Mnemos-Experiment-Id`) and the bearer token. Only official releases have an updater at all (`MNEMOS_OFFICIAL_RELEASE=1` in the release scripts); a build from source has none. See `RELEASING.md`. | `689c5def3`, `c2fac2cde` |
| Telemetry (RudderStack client) | `telemetry_config: null` in `app/build.rs`, and `ChannelState::is_telemetry_available` also needs the non-default `telemetry` cargo feature. Batches are dropped before any request (`send_batch_messages_to_rudder` in `app/src/server/telemetry/mod.rs`) and nothing is written to disk. `privacy.telemetry_enabled` defaults to false, the toggle is hidden, AI content is never collected, and the free-tier "enable analytics to use AI" gate is off. The `brew list` download-source probe that only fed a telemetry event no longer runs. | root commit, `6d557aeb4`, `5a28ca85f`, `324e353a1`, `f1f3a882f`, `8f86e26a5`, `d7f07b53d` |
| Crash reporting (Sentry) | `crash_reporting_config: null`, so the DSN is empty. The `crash_reporting` cargo feature is off in every bundle script (macOS since `f1785c17c`, Windows and Linux since `d5fa4251e`), so the Sentry SDK is not compiled into stable builds. The macOS dev and preview channels still pull it in through `heap_usage_tracking`. `privacy.crash_reporting_enabled` defaults to false and its toggle is hidden. | `f1785c17c`, `42397c400`, `d5fa4251e`, `f1f3a882f` |
| AgentModeAnalytics telemetry override | A server experiment could set `FeatureFlag::AgentModeAnalytics`, which turned telemetry and AI content collection on even when the user had turned telemetry off. That override is gone, and so are server experiments. | `f1f3a882f`, `2670c9b72` |
| Upstream GraphQL pollers and the AI model list refresh | Skipped under the Mnemos sync login (`has_upstream_server_session`, `ServerApi::send_graphql_request`). | `49498fadc`, `c9bbc4727` |
| Cloud-object update websocket | Opened only with an upstream server session (`Listener::start_listener` in `app/src/server/cloud_objects/listener.rs`). | `c81124201` |
| "Store AI conversations in the cloud" setting | Hidden in Settings > Privacy without an upstream server session. | `f1f3a882f` |
| Warp Drive cloud sections | Hidden; the Drive panel shows only Mnemos's local sections when signed out. | `340046c28` |
| Teams, referrals, Mnemosify, cloud environments and Oz cloud API key settings pages | Not in the settings sidebar, and `SettingsSection::is_hidden` keeps every other route from opening them (`app/src/settings_view/mod.rs`). | root commit |
| Onboarding | Defaults to the terminal intention; Drive needs no login. The upstream wizard is not initialised (`onboarding::init` is commented out in `app/src/lib.rs`). | `034f503e3` |

## Still compiled in

Upstream auth, cloud objects and GraphQL, Drive and the AI model list are still compiled in, and
so is the telemetry client code (inert without its feature and config). With the destinations
above, they either stay idle or call `server_root_url`, which has no upstream `/graphql`.
`ARCHITECTURE.md` ("Upstream code that is still compiled in") lists the plain HTTP calls that are
not gated.

## Network at startup

On a signed-out launch, nothing in Mnemos IDE contacts a server before the first window has drawn
a frame (`d7f07b53d`, `c81124201`). `AppContext::on_first_frame_drawn` chains callbacks, and the
update check and the window-launch changelog fetch wait for it. What can still go out at or around
startup:

| Request | Where | When |
| --- | --- | --- |
| Update check: anonymous `GET {releases_base_url}/channel_versions.json` | `AutoupdateState::register` (`app/src/autoupdate/mod.rs`), request built in `app/src/autoupdate/channel_versions.rs` | Official releases only: a minute after the first frame, then every 10 minutes and on app activation |
| Changelog: the same manifest request | `app/src/autoupdate/changelog.rs`, held by `ChangelogModel::new_deferred` until the first frame | Official releases only, and at launch only when this version has not been launched before here |
| Server time: `GET {server_root_url}/current_time` | `ServerApi::server_time`, asked for from `app/src/root_view.rs` | Only after an update check finds an update with an update-by time |
| User refresh (`GetUser` and token refresh) | `AuthManager::refresh_user`, from `app/src/lib.rs` | Signed in only, during initialization |
| `mnemos-ide-sync` (pull, then push, then the 30 s poll) | `AuthManager::start_ide_sync` (`app/src/auth/auth_manager.rs`) | Signed in to the Mnemos sync server only, during initialization |
| MCP servers the user configured | `TemplatableMCPServerManager::new` restores them (`app/src/lib.rs`) | Whenever the user has MCP servers to restore; their own traffic |
| Codebase index restore | `CodebaseIndexManager::new` (`app/src/lib.rs`) | Only when codebase context is enabled and indices were persisted |
| `LoggedOutStartup` telemetry event | `app/src/lib.rs` | Queued, then dropped before any request (no telemetry feature or config) |

This list comes from reading the code, not from a packet capture. The in-app network log
(Settings > Privacy) shows what a running build actually sends.
