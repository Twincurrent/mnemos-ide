# Mnemos IDE: Architecture

How Mnemos IDE is put together today. For build steps and contributor rules see `AGENTS.md` and
`CONTRIBUTING.md`; for releases see `RELEASING.md`; for what was removed from Warp see
`docs/upstream-diff.md`.

> **Upstream.** Mnemos IDE is a fork of the [Warp terminal](https://github.com/warpdotdev/warp),
> cloned from Warp's public repository on 2026-04-29. We accept divergence and have no rebase
> commitment. See `NOTICE` for attribution and licensing.

> **Renamed 2026-10-08.** The `tcb/tcb-*` crates are now `ide/mnemos-ide-*` (libs
> `mnemos_ide_*`), `TcbError` is `IdeError`, `tcb_*` functions are `ide_*`, `script/tcb-check` is
> `script/ide-check`, and this file was `TCB_ARCHITECTURE.md` (TCB was the name of the earlier
> Electron app, also called v1). User-settable `TCB_*` env vars became `MNEMOS_*`; the old names
> are still read (`mnemos_ide_core::paths::env_var_os`). Wire names did not change (`/api/tcb/*`,
> every sync state key, the `claude-sessions/` blob prefix), nor did on-disk names (the `tcbv2` /
> `tcbv2-<profile>` / `tcbv2-isolated` keyring services, the `Mnemos` data dir, the
> `tabs.tcb_project_id` SQLite column, `.twincurrent-build/`, the legacy seed paths and the pull
> backup dir `~/.claude/backups/tcb-sync-<date>/`). `AGENTS.md` "Names that kept `tcb`" has the
> full list and the reasons.

---

## The shape of it

Almost all of the code is upstream Warp, rebranded: the GPU-rendered block terminal, the UI
framework, the editor, the agent harness and the platform layers. Mnemos IDE adds a thin layer
of its own crates under `ide/` and wires them into the app with small, marked edits (comments
starting `Mnemos IDE:`). It also removed or compiled out a good part of Warp's cloud product;
`docs/upstream-diff.md` lists what and where.

```
mnemos/
├── Cargo.toml                 # workspace root: crates/*, app, and the 8 ide/* crates
├── app/                       # the main binary (`mnemos`), plus the Mnemos IDE wiring
│   └── src/
│       ├── bin/mnemos.rs      # entry; dev isolation defaults, sets Channel::Stable, calls lib.rs::run()
│       ├── lib.rs             # app boot; registers upstream secure_storage, agent restore
│       ├── workspace/         # ide_sync.rs, live_agents.rs, view/{left_panel,ide_tab_sync,ide_sessions,agent_suggest}.rs
│       ├── auth/              # upstream auth + the sync sign-in (ide_sync_auth.rs, ide_sync_e2e.rs)
│       ├── settings_view/     # sync_page.rs (+ sync_page/e2e.rs), sync_privacy.rs: Settings > Sync
│       ├── drive/index.rs     # Drive panel: the ~/.claude sync status rows
│       ├── autoupdate/        # the signed-manifest updater
│       └── terminal/          # local_tty/ (the live PTY), cli_agent_sessions/ (agent events)
│
├── crates/                    # upstream crates, mostly as-is; channel_versions holds update signing
│
├── .claude-plugin/            # marketplace.json pointing at ide/claude-plugin/plugins/mnemos-agents
│
└── ide/
    ├── mnemos-ide-core/              # Project type, paths (data dir, dev isolation), registry merge
    ├── mnemos-ide-projects/          # project store, ProjectTabs, resolve.rs, reconcile.rs
    ├── mnemos-ide-sync/              # sign-in, state + transcript sync, gates, E2E, live feed
    ├── mnemos-ide-agents/            # agents panel: running agents, past Claude sessions
    ├── mnemos-ide-sessions/          # reads Claude Code session transcripts; Suggest's pure half
    ├── mnemos-ide-agent-restore/     # resume claude sessions in the same pane after a restart
    ├── mnemos-ide-git-tree/          # git panel in the code-review header
    └── claude-plugin/                # Claude Code plugin (mnemos-agents hooks), not a crate
```

**Convention:** our crates live under `ide/` and are named `mnemos-ide-*` so they cannot collide
with upstream's `mnemos_*` crates (Cargo treats `-` and `_` alike). The directory boundary keeps
the diff against upstream readable: anything outside `ide/` is upstream as-is or a small,
marked edit. Every `ide/` crate has `#![forbid(unsafe_code)]` and
`#![deny(clippy::undocumented_unsafe_blocks)]` in its `lib.rs`.

---

## Crates

### `mnemos-ide-core`
Shared types and paths. No async, no I/O beyond creating the data dir.
- `types.rs`: `Project` and the other shared types.
- `paths.rs`: `ide_data_dir()` (`<platform data dir>/Mnemos`, or `$MNEMOS_DATA_DIR`, or the
  legacy `$TCB_DATA_DIR`), the store files under `store/`, the data profile, the env var helpers
  that read each `MNEMOS_*` name with its `TCB_*` fallback, and the dev isolation defaults
  (`apply_dev_isolation_defaults`, see "Build, test and CI").
- `registry.rs`: the cross-machine merge for `projects.json`, and `claude_project_slug` (Claude
  Code's own rule for `~/.claude/projects/<slug>` names).
- `IdeError` (thiserror).

### `mnemos-ide-projects`
The project store and the open-project state behind the native tab strip. Every native workspace
tab is a project (each tab carries a `tcb_project_id`).
- `store.rs`: `ProjectStore` loads and persists `<data dir>/store/projects.json`. On first run,
  when the file is missing and the data dir is not redirected, it seeds once from the pre-rename
  `twincurrent-build-ui/store/projects.json`, or else the v1 `TwinCurrent Build/store/projects.json`.
- `project_tabs.rs`: `ProjectTabs`, the open projects plus the active id. No rendering.
- `resolve.rs` and `reconcile.rs`: see "Cross-machine projects".

### `mnemos-ide-sync`
Sign-in and state sync against a Mnemos sync server, plus `notify` file watching. Today the only
server is the one the makers run (the URL is baked in by `app/build.rs`; a source build that is
not a `release_bundle` can point elsewhere with `MNEMOS_SERVER_ROOT_URL`); self-hosting is
planned. Modules: `auth.rs` and `desktop_auth.rs` (sign-in), `client.rs` and `teams.rs` (HTTP),
`protocol.rs` (wire version), `manager.rs` (the sync loop), `merge.rs`, `watcher.rs`,
`transcripts.rs`, `gates.rs`, `settings_toml.rs`, `crypto.rs` and `e2e.rs`, `live_agents.rs`,
`machine.rs`, `team_choice.rs`, `options.rs`. See "Sync" below.

### `mnemos-ide-agents`
The tools-panel list of running agent sessions (Claude Code, Codex, Gemini and OpenCode),
grouped by project, fed by
`app::terminal::cli_agent_sessions::CLIAgentSessionsModel` and by other machines' live-agent
snapshots, and its SESSIONS section: the past Claude Code sessions of every project (see "Claude
sessions" below). See `AGENTS.md` for the interaction rules.

### `mnemos-ide-sessions`
Reads Claude Code's own transcripts (`~/.claude/projects/<slug>/<uuid>.jsonl`): lists a project
root's sessions, subfolders included (`SessionStore::list_for_root`), and pulls a cheap label out
of each head, cached by `(mtime, len)`. The slug rule is `mnemos-ide-core`'s, the one transcript
sync files pulled sessions under. `suggest.rs` is the pure half of the agents panel's Suggest
(see "Suggest"). No UI, storage, sync or network of its own.

### `mnemos-ide-agent-restore`
Carries live `claude` sessions across an app restart: stores pane uuid -> Claude session id (and
cwd) in the local-only `store/agent_sessions.json` and resumes each in the same pane on the next
launch. Consume-on-read, validated and capped, so a bad restore cannot loop. A session another
machine may still be running comes back as a fork (`--fork-session`, a new id) instead, because a
second `claude --resume` of one id takes that machine's Remote Control session over (code 4090).
Signed in, the panes get their command once both the first live-agents answer and the first
transcript pull of the process are in (`SyncStatusSnapshot::first_transcript_pull_done`), at most
10 s after the first restore of the process; signed out, at once (`live_agents_wanted` reads
right away, it does not wait for the polling to start). Then `guard::resume_mode` forks when
`agents-live` lists the id, or when the transcript's newest turns were pulled from another machine
(`transcripts::pulled_origins`, file still at that length) after this machine last had the
session open: the snapshot's write time, or an entry's own `last_live_unix`. The pull is dated by
when it ran here (`PulledOrigin::at`, `pulledAtMs` in the transcript sync state), not by the
file's mtime, which a merge sets to the newest line's time by the other machine's clock. Turns
pulled before that point predate this machine's own run (transcript sync never appends to a
session a `claude` here has open), so they resume in place whether or not a list was read. A list
is definitive only for machines that publish one (`agents_live_for`): the live feed is off by
default, so a machine missing from the lists is "unknown", not "not running it". Another machine
is told from this one by key and by `machine_id` (`live_agents::others`), so a host name that
changed under a running Mac does not make its own previous snapshot look like another machine. A
pane gets its command only once its shell is up (its bootstrap precmd is in), never parked during
startup, where whatever the user typed before the shell was ready would run with it
(`restore_pane_step`, looked at every 100 ms, at most 60 s). A pane the user typed in or ran a
command in meanwhile, or whose shell did not come up in time, gets nothing: the entry is kept
(`queue::defer`) and written into the next snapshot, and the session stays in the sessions list.
Each pane goes on its own, so the panes the user has not touched are restored as soon as their
shell is up, whatever happens in the focused one. To keep the wait short, the first transcript
pull of the run is queued before the initial push and runs ahead of the startup sweep (the
pull uploads a file's own unpushed lines before merging into it, so pulling first loses
nothing). A resume from the sessions list drops a kept entry only when it launched that session
in place, not for a fork. The snapshot is
written in `on_will_terminate`, which a quit, an autoupdate relaunch and, on Windows, a shutdown,
restart or logoff all reach: a hidden top-level window in `mnemos_ui`
(`windowing/winit/windows/end_session.rs`) answers `WM_QUERYENDSESSION` with TRUE and runs the
terminate step inside `WM_ENDSESSION`, and the process asks for shutdown level 0x300
(`windowing/end_session.rs`) so Windows ends it before its shells. During a session end no update
installer or relaunch is started.

### `mnemos-ide-git-tree`
The git panel in the code-review header (it replaced Warp's `DiffSelector`): branches, remotes,
tags, open pull requests (through the `gh` CLI) and the commit graph, with safe checkout.
`model.rs` wraps `git2`, `lane_router.rs` assigns graph lanes, `prs.rs` parses `gh pr list`,
`view.rs` renders. `AGENTS.md` "The git tree panel" has the details.

### `mnemos-ide-team`
The UI-free half of team sessions (see "Team sessions"): validation, the launch command, spawn
caps, leases, git worktrees, the team board reader and the MCP messages. Unit-tested on its own,
worktrees against scratch repositories.

### `ide/claude-plugin`
Not a crate: a Claude Code plugin marketplace whose `mnemos-agents` plugin has hooks that stream
session events (status, prompts, tool and permission state, Remote Control URL, background task
count) to the agents panel. It is the plugin Mnemos installs. The repo root's
`.claude-plugin/marketplace.json` points at `ide/claude-plugin/plugins/mnemos-agents`, so the app's
install action (`app/src/terminal/cli_agent_sessions/plugin_manager/claude.rs`) adds the `mnemos`
marketplace from the public source repository (`script/source-url`) with a sparse checkout of
those two paths; `script/install-claude-plugin` registers a local checkout instead. Updating from
the app refreshes the registered marketplace in place (`claude plugin marketplace update mnemos`,
then `claude plugin update mnemos-agents@mnemos`) and never removes it; only when no `mnemos`
marketplace is registered does it add one. Re-pointing a marketplace whose directory moved stays a
manual step (`AGENTS.md`). From 1.2.0 the plugin sends this terminal's token with every event
(see "CLI agent events"). From 1.3.0 it also carries team rules and a board (`scripts/team.sh`,
`on-team-context.sh`). The marketplace's second plugin, `mnemos-team`, only declares the team MCP
server (`"Bearer ${MNEMOS_CLI_AGENT_TOKEN}"` against `${MNEMOS_TEAM_MCP_URL}`).

### Team sessions
A Claude Code session in a Mnemos pane (the coordinator) can start worker sessions in panes next to
it and manage them. Three layers:

- **Rules and board, no app needed.** `<git common dir>/mnemos-team/rules.md` and `board.json`
  are shared by every worktree of a repository and never committed. The mnemos-agents hook
  `on-team-context.sh` adds a changed rules file or board to each session's next turn, once per
  change, and all of it again on SessionStart. Sessions update the board with `team.sh set`. The
  agents panel shows the active project's board as TEAM, re-read on its sessions tick.
- **The team MCP server** (`app/src/workspace/team_control.rs`). With `agents.team.enabled` the
  app listens on a random loopback port and exports `MNEMOS_TEAM_MCP_URL` to new terminals. Each
  request's bearer token is looked up in `CLIAgentEventFileIngestor` (`terminal_view_for_token`),
  which names the calling pane and its tab; an unknown token gets 401, any `Origin` header or a
  foreign `Host` 403. Answers are plain `application/json`, notifications 202.
- **The tools.** `spawn_session` splits a pane to the right of the caller
  (`PaneGroup::ide_split_terminal_running`) in the caller's own tab, optionally in a new git
  worktree, and runs `claude --name <name> [--model m] [--dangerously-skip-permissions]
  "$MNEMOS_TEAM_BRIEF"` once the shell is up, with the brief and `USE_BUILTIN_RIPGREP=0` in the
  pane's environment. `list_sessions`, `focus_session`, `rename_session` and `retire_session` act
  on the sessions in the caller's tab; retire refuses while the session is working, checks the
  worktree first (dirty or unmerged: refused, nothing closed), then closes the pane and removes the
  worktree and its merged branch. `list_worktrees` and `prune_worktrees` report on and clean the
  repository's worktrees. `lease`, `release` and `list_leases` hand out cpu, chrome and port
  leases that end on release, expiry or when the holder's pane closes.

Guards: the first spawn from a pane opens an Allow/Deny dialog, remembered while the pane lives;
spawned panes can never spawn; at most 6 spawned sessions live and 3 spawns a minute; every spawn
shows a toast and its brief is the session's first prompt on screen; every call is logged under the
`mnemos_ide_team` target. `agents.team.spawn_skip_permissions` (local, never synced, off by
default) is the only way a spawned session skips permission prompts. `agents.team.keep_awake`
(local, off by default) keeps the computer from idle sleep while any agent works. Messaging between
sessions is Claude Code's own (SendMessage), not this server's.

---

## Cross-machine projects

One registry, the same tabs on every machine.
- A record carries `paths: {darwin, win32, linux}` (its checkout on each platform), `gitRepo`
  (`owner/repo`), and `open` + `openChangedAt`. `open == true` records are the tab set on every
  machine; `path` is kept for v1 compatibility only. Never use `project.path` in app code: use
  `Project::local_path()` or `resolve::LocalResolver`.
- `mnemos_ide_core::registry::merge` is a deterministic union by id with per-field clocks and
  `deletedProjects` tombstones (`replacedBy` for folds). Records that share a `gitRepo` and claim
  no conflicting platform path fold into the oldest one. Push and pull both use it, so neither
  machine can erase the other's projects.
- `resolve.rs` finds a record's checkout on this machine by git remote (read from `.git/config`,
  no git CLI) or folder name under the usual code roots, and persists `paths.<platform>`. It never
  scans macOS privacy folders (Desktop, Documents, Downloads, ...). The discovery scan is cached
  for 5 minutes, so a repo cloned after launch is found by the next reconcile without a restart.
- `reconcile.rs` is a pure planner: it links each tab to a record (stored id, then cwd root,
  remote, name), learns paths, and returns tabs to open or close so the tab strip matches the
  registry. The app (`app/src/workspace/ide_sync.rs`, `view/ide_tab_sync.rs`) runs it at window
  restore (after the first sync pull) and whenever `projects.json` changes (polled every 2 s),
  off the UI thread, one job at a time; only the oldest window opens missing tabs.
- Closing a project's last tab sets `open = false`; it never deletes the record. Reconcile only
  closes a tab that another machine closed when the tab is idle (no running command or agent).
  Plain tabs (no project id) never become projects on their own.
- `workspace_state.json` (active project, tab order, the per-project sync opt-out
  `syncDisabledProjectIds`) is local-only.

---

## Sync

Everything in this section needs a signed-in account on a sync server. Without one, Mnemos IDE
runs fully locally and none of it happens. The per-machine controls are in Settings > Sync
(`app/src/settings_view/sync_page.rs`, `sync_privacy.rs`).

### Sign-in
- **Desktop sign-in uses PKCE** (`desktop_auth.rs`, RFC 7636 S256). The app opens the server's
  `/signup/remote` page with a `code_challenge` and a random `state`. After the user signs in,
  the browser hands the app `mnemos://auth/desktop_redirect?code=...&state=...`: a one-time code,
  not a token. The app trades it for the token pair with `POST /api/auth/desktop-exchange`, proving
  with the `code_verifier`, which stays in memory in this process and is never written anywhere.
  The server fixes the redirect scheme to `mnemos`. A JWT the app cannot decode fails the sign-in.
  Server side, a client that sends no challenge (every build from before PKCE) still gets the
  tokens in the redirect URL; that branch is to be removed after 2027-01-31
  (`LEGACY_TOKEN_REDIRECT_REMOVAL` in the Admin repo), and the app still accepts such a redirect
  until then.
- `auth.rs`: the token pair is stored with the `keyring` crate. The service is `tcbv2` for the
  installed app; `MNEMOS_DATA_PROFILE` builds use `tcbv2-<profile>`, a redirected data dir
  `tcbv2-isolated`, and tests an in-memory store, so no dev build or test can touch the daily
  login. The names are deliberately unchanged: the server rotates the refresh token on every
  refresh, so a login copied to a new service would fork the token and sign one build out.
- Auth state is part of `SyncStatusSnapshot`. A refresh the server rejects (401/403) means
  SignedOut and all sync traffic stops; the workspace header shows a "Sync signed out" chip and
  Settings > Sync a row, both of which start sign-in again without a restart. A token that could
  not be saved to the keyring surfaces as TokenNotSaved.
- The sign-in sets upstream's `Credentials::SessionCookie`, so upstream's `is_logged_in()` is
  true. Under it, `ServerApi::send_graphql_request` bails before any network call and
  `has_upstream_server_session` skips the upstream cloud-object, workspace-metadata and model-list
  pollers and the cloud-object websocket (`app/src/server/server_api.rs`).
- Upstream's Firebase sign-in is gone: a browser redirect with neither a code nor sync tokens, and
  the CLI device-code login, fail with a message (`AuthManager::refuse_firebase_sign_in`).

### Protocol (`protocol.rs`)
- Every request carries `X-Mnemos-Sync-Protocol: 1` (`SYNC_PROTOCOL_VERSION`). Builds from before
  the header send none, which the server counts as 0. The server refuses a version below
  `MNEMOS_SYNC_MIN_PROTOCOL` (default 0, so nothing is refused until an operator raises it) with
  `426 Upgrade Required` and a JSON body; the client then stops syncing and shows "update Mnemos to
  keep syncing" (`SyncAuthState::UpdateRequired`). Updating Mnemos is the only way out. A 426
  without that body is treated as any other HTTP error.
- A `{schema, data, tombstones?, deleted_at?}` value wrapper is defined for keys introduced from
  protocol 1 on, with the rule that a reader never applies or overwrites a value whose schema it
  does not know. No key uses it yet (`ENVELOPE_KEY_PREFIXES` is empty): the keys added since,
  `mnemos-user-settings-v2`, `claude-json-mcp-servers-v2`, `e2e:*` and `e2e-meta`, are stored bare.
  Keys older builds read keep their exact format and in-band sentinels (`__deleted`, ...).

### State keys
- `client.rs`: HTTP client for `/api/tcb/state/*` with compare-and-swap versioning
  (`SYNC_API`), the blob store `/api/tcb/blobs`, `GET /api/tcb/me` and the deletion calls.
  A 401 refreshes and retries once; a 409 is a typed CAS conflict.
- `manager.rs` does per-key debounced push, pulls and apply; `merge.rs` holds the per-key merge
  (directory union with `__deleted` tombstones, memory mtime-win, the registry merge, the session
  index union, local-wins for the rest).
- What syncs: `projects`, `settings`, the Mnemos `settings.toml` (per key, see below), the
  `~/.claude` globals and directories listed in `GLOBAL_FILES` / `DIRECTORIES` in `manager.rs`,
  `claude-memory:*`, `claude-json-mcp-servers-v2`, and Claude Code transcripts (below).
  - `~/.claude/.env` syncs only on machines that turned it on in Settings > Sync (off by default,
    kept in the local `store/sync_options.json`). While it is off, Settings > Sync offers to remove
    the server's copy; nothing removes it automatically, because builds from before the opt-in
    still sync it.
  - `claude-json-mcp-servers-v2` carries `~/.claude.json`'s `mcpServers` without each server's
    `env` and `headers`. The old `claude-json-mcp-servers` key is never applied, and a push removes
    this platform's slot from it.
  - `settings.json` `env` entries whose names look secret (`KEY`, `TOKEN`, `SECRET`, `PASSWORD`,
    ... as a name segment, `gates::is_secret_env_name`) are never pushed or applied. A push carries
    the server's own values for those names, so older builds lose nothing; local values always win.
- What does not: `.twincurrent-build/` (nothing pushes it, and `apply_state` skips
  `twincurrent-build:*` keys), and every file in `store/` (`workspace_state.json`,
  `agent_sessions.json`, `transcript_sync.json`, `sync_gates.json`, `sync_options.json`,
  `team_choice.json`, `machine_id`).
- `projects`: push and pull both run `registry::merge`; pull writes through
  `ProjectStore::update_blocking_at` under the store lock and pushes back only when the merge
  adds something the server lacks. The key is polled every 30 s (applied only on a new version),
  with a full pull every 10 minutes.

### Watcher (`watcher.rs`)
`notify` with a 500 ms debouncer; the manager adds its own per-key debounce. The rules that keep
it from going quiet:
- Every target is watched, present or not. A missing one (no `~/.claude/agents` yet) is anchored
  on its nearest existing ancestor and re-armed when it appears, with a synthetic event so its
  contents are pushed; one that disappears falls back to its anchor. A recheck every 30 s catches
  what the anchor watch missed.
- Files are watched through their parent directory (except on macOS, where FSEvents follows a
  file across atomic replaces), so a write-to-temp-then-rename does not drop the watch.
- Event paths are mapped back onto the configured spelling (FSEvents reports canonical paths such
  as `/private/var/...` or a resolved symlink).
- One bad path never takes the watcher down: a failed watch is logged once and retried on the
  recheck, and a path refused for want of kernel resources (inotify's `max_user_watches`, ENOSPC,
  EMFILE) or an unsupported backend moves to a polling watcher (every 10 s).

### Approval gates (`gates.rs`)
Some synced content runs code on the machine it lands on: hook scripts, the command and permission
keys of Claude Code's `settings.json` / `settings.local.json` (`hooks`, `statusLine`,
`apiKeyHelper`, `env`, `permissions`, ...), `statusline.py`, MCP server scripts under
`~/.claude/mcp-servers/`, `mcpServers` entries, and agent, command and skill files (their
frontmatter can grant tools or carry hooks). Whoever can write the server's copy, or a team's,
could otherwise run code on every machine that pulls it.
- Hooks sync is opt-in per machine, off by default. While off, `claude-hooks` and
  `claude-statusline` are neither pushed nor pulled, and the command keys of `settings.json` are
  frozen (a pull keeps this machine's, a push keeps the server's).
- Agents, commands and skills always sync, but every change to them is gated.
- A gated item a pull would change, add or delete is written only once its content hash was
  approved on this machine (Settings > Sync lists each pending item with a diff). Until then this
  machine's copy stays, and a push sends the server's copy, not this machine's older one.
- Team content is gated the same way. Prompt content (CLAUDE.md, rules) is not gated.
- Approvals, dismissals, pending items and the opt-in live in the local `store/sync_gates.json`.

### `settings.toml` (`settings_toml.rs`)
Only settings the app marks `SyncToCloud::Globally` or `PerPlatform`
(`app/src/settings/ide_sync_policy.rs`) sync, minus `DENIED_SETTINGS` (settings that name a program
to run, grant the agent execution rights, the two command denylists and
`privacy.secret_redaction.enabled`), as JSON under `mnemos-user-settings-v2`
(`{"v": 2, "global": {...}, "darwin": {...}, "win32": {...}}`). The merge is a union: a push keeps
every server entry this machine does not set, a pull writes the server's entries and removes
nothing, so resetting a setting to its default does not propagate. The old whole-file key
`mnemos-user-settings` is left as older builds wrote it, neither pushed nor applied, so settings
changes do not cross between an old build and a new one.

### End-to-end encryption (`crypto.rs`, `e2e.rs`)
Optional, turned on per account in Settings > Sync.
- The first machine generates a random 256-bit key and shows it once as a recovery key; every
  other machine needs it typed in. It is kept per admin profile in upstream secure storage (macOS
  login keychain, DPAPI file on Windows; item `MnemosSyncRecoveryKeys`, `app/src/auth/ide_sync_e2e.rs`),
  never in the `tcbv2` keyring entry, and never sent anywhere.
- Every personal state value and transcript blob is sealed with XChaCha20-Poly1305 (random 192-bit
  nonce; key and key id derived with HKDF-SHA256; the associated data binds the envelope version,
  key id and key name or blob path) and stored under `e2e:<key>` / `e2e/<path>`, a namespace older
  builds never read. A plaintext `e2e-meta` marker holds the key id, a sealed check value and the
  creation time. Team-scope state and `agents-live:*` stay plaintext.
- `e2e.rs` routes the client's state and blob calls to the encrypted namespace, so the rest of the
  crate only sees logical names. State envelopes carry a `seq`; a value more than 5 minutes older
  than one already seen in this run is refused (a replay guard that resets at restart).
- Turning it on migrates existing data: during a 24-hour window sealed into the marker, a full pull
  merges plaintext under a personal name into the encrypted copy and deletes the plaintext with a
  conditional delete (`DELETE /state/{key}?expected_version=N`, refused with 409 when the row moved
  on), and a transcript listing re-uploads plaintext blobs encrypted and deletes them (blobs have no
  versions). After the window plaintext is ignored.
- Statuses: Off, On, NeedsKey, WrongKey, Unsupported, MarkerMissing. A machine with a saved key
  that finds no marker does not fall back to plaintext: MarkerMissing pauses personal sync like a
  missing key, until the marker is put back with the saved key. Settings > Sync also offers
  "Start over" (forget the key here and sync unencrypted).
- The server still sees key names (memory keys include `<owner>/<repo>`), sizes, versions, times
  and blob paths. Database backups and blob retention may still hold plaintext from before.

### Teams (`teams.rs`, `team_choice.rs`)
A Drive team is the sharing unit for `~/.claude` config: agents, commands, skills, rules,
CLAUDE.md and hooks are mirrored into team-scoped state (`/api/tcb/teams/:id/state`), which the
server stores readable, not end-to-end encrypted. Being a member on the server is not enough: each
machine takes part only in the team chosen there (`store/team_choice.json`), and team hooks pass
the approval gates. A machine that synced before the choice existed is recognised by either a
`sync_baseline.json` from an earlier run or an admin login already in the keyring at startup, and
keeps its first listed team; a fresh sign-in does not count, and that first run records "no team".

### Claude memory
- `manager.rs` watches `~/.claude/projects/<slug>/memory/` and syncs it under
  `claude-memory:{profile}:{gitRepo}`.
- The memory dir to repo mapping is per machine. `manager.rs` resolves `paths.<platform>` first,
  then `path`; when neither exists locally it reads `cwd` from the dir's transcripts, matches
  `git remote get-url origin` (normalised to `owner/repo`) against `gitRepo`, and persists the
  result. Dir names use Claude Code's slug rule (every char outside `[A-Za-z0-9]` becomes `-`) and
  never fall back to the other machine's path.

### Transcripts (`transcripts.rs`)
Syncs Claude Code sessions (`~/.claude/projects/<slug>/<uuid>.jsonl`, top level, last 30 days)
through the server's blob store (`/api/tcb/blobs`), so `claude --resume <sid>` works on every
machine.
- **Layout:** `claude-sessions/{sid}/{machine}.meta.json` plus append-only gzip segments
  `{machine}-{seq}.jsonl.gz` (at most 4 MiB raw, complete lines only). `{machine}` is the random id
  in the local `store/machine_id` (`machine.rs`).
- **Pull** appends other machines' segments to the one local copy of `<sid>.jsonl` (deduped by
  line `uuid`), rewriting each line's top-level `cwd` to this machine's equivalent. A session
  Claude Code has live on this machine is never written.
- **Where a session belongs**, by its first cwd, best match first: a registered project; one of
  the usual code folders (`mnemos_ide_projects::resolve::code_roots`, matched by folder name,
  case-insensitive, so `/Users/alice/code` and `C:\code` line up); under home (mapped onto the
  other machine's home, so `~/projects/foo` on one maps to `~/projects/foo` on the other); else
  the absolute cwd kept unchanged (listed and readable everywhere, resumable only where that path
  exists). Each machine's meta is tried on its own and the best place wins.
- **Never synced:** sessions whose cwd is inside `~/.claude`, the Mnemos data dir or a system
  temp folder (`is_temp_cwd`), and sessions of a project whose sync is turned off on this machine
  (`syncDisabledProjectIds`), in either direction. A pulled meta with a `..` or `\` in `relCwd`, or
  a relative or `..` origin root, is ignored.
- **Deletes:** the startup sweep deletes this machine's own uploads (meta and segments) of sessions
  in a project that is opted out here; other machines' copies, and copies already pulled
  elsewhere, stay. The encryption migration deletes plaintext blobs it re-uploaded. Nothing else
  on the client deletes blobs.
- **No surprise disk access:** pull only builds the mapped path as a string, and marks
  transcripts it created as `pulledHere` in `transcript_sync.json`, so the memory checkout scan
  never stats their cwd (no macOS privacy prompt, no stall on an absent network volume).
- **Delivery:** after uploading, the worker adds `{"<sid>": {"<machine>": {"seq", "at"}}}` to the
  `claude-session-index` key through the normal CAS push (union of sessions and machines, higher
  `seq` wins, entries 3 days older than the newest dropped). The 30 s poll reads the index next to
  `projects` and pulls only the sessions it names with unapplied segments; an hourly full listing
  and `pull_now` are the safety net, and a cheap sweep each cycle uploads what the watcher missed.
  `SyncManager::stop` aborts the transcript worker and waits for it to end, so nothing from a
  stopped run uploads afterwards.
- **Oversized lines:** a segment refused with 413 is resent line by line; a single refused line
  goes up as a stub (same JSON keys, strings over 64 KiB replaced by a size note, base64 image or
  document blocks by a text block), so the `parentUuid` chain stays whole.
- Local state: `store/machine_id` and `store/transcript_sync.json`, never synced.

### Claude sessions (agents panel)
`app/src/workspace/view/ide_sessions.rs` feeds the agents panel's SESSIONS section and resumes
sessions.
- **Listing:** every project with a checkout here (`local_path`), plus the active terminal's
  folder when no project holds it. A session started in a subfolder lists under its project
  (`list_for_root` reads the root's slug folder and every `slug(root) + "-"` folder, keeping a
  session only when its `cwd` is under the root by whole components). Nested projects each keep
  their own sessions: the deepest root claims its sessions first (all of them, past its cap too),
  and the cap applies after that, so a busy nested project never crowds out the outer one's. At
  most 20 per project are listed and five shown until "more". The panel draws SESSIONS after the
  live agents of this machine and the other machines.
- **Cross-machine:** nothing extra. Transcript sync already pulls the other machine's sessions
  into this machine's checkout, `cwd` rewritten, so they list and resume like local ones. A pull
  that appended lines records the sending platform and the file length it left
  (`transcripts::pulled_origins`, local `transcript_sync.json` only); while the file still has
  that length the row says "from Windows" (or Mac). A project with no checkout here has nothing
  on disk; its running sessions show under the other machine in the panel instead.
- **State per row**, worked out at push time: running here (a `CLIAgentSessionsModel` session has
  its id; both clicks switch to it), running on another machine (its id is in that machine's
  `agents-live` snapshot, `LiveAgent.id`; only a fork from what has synced, plus `open` with a
  Remote Control URL, never an in-place resume, which would make two writers on one session),
  folder missing (the mapped `cwd` is not a directory here; nothing to run), pulled recently (no
  machine lists it, but its newest turns were pulled from another machine less than 10 minutes
  ago, `RECENT_ELSEWHERE`, counted from when the pull ran here, and nothing ran here since: that
  machine's `claude` may still run unseen, its snapshot stale or never published, so both clicks
  fork and the row keeps its "from Windows"), else resumable (click: in place, right-click:
  `--fork-session`). A plain click re-checks first (a fresh stat of the transcript, the pull
  record and the current live lists, `click_mode`): the scan can be 30 s old, and a pull since
  then turns the click into a fork. Until a window has had one live-agents answer
  (the registry keeps the last one and hands it to each new window), in-place resume is refused
  and only a fork runs; offline from the start, the registry answers "no other machines" after
  its first failed read. With the live feed off nobody publishes, so other machines' sessions
  cannot be seen as running.
- **Scans** run off the UI thread: when the panel comes into view, on its refresh chip, every
  30 s while it is visible, and when a local agent starts or ends while it is visible; at most
  one every 5 s unless asked for. One parse cache serves every window.
- **Resume opens inside the project**, never as a new project tab when one exists: a new terminal
  tab (`PaneGroup::ide_add_terminal_tab_running`, stacked into the focused pane's group) in the
  tab that carries the project's id, else a tab with a terminal under its root that is not
  linked to another project, preferring the active tab, in this window and then in the others
  (that window is raised). Only a registered project with no tab anywhere gets one
  (`ide_add_project_tab` plus `IdeJob::TabReopened`, so the reconcile does not open a second). A
  session outside every project opens in the active tab. The command is `claude --resume <id>`
  (`is_safe_session_id` checked again), queued with `execute_command_or_set_pending` like agent
  restore. A second click within 4 s of a launch is ignored by the panel; after that, while the
  terminal a window launched the session in still has the command pending or running, a click
  switches to that terminal instead of starting a second `claude` on the same transcript (the
  agent hook can take longer than 4 s to report it, and never does without the plugin).
- **Repaint:** the panel's own age timer runs only while agents are listed; session ages move on
  with the workspace's 30 s sessions tick, which only runs work while the panel is on screen.

### Live agents (the live feed)
- `mnemos-ide-sync/src/live_agents.rs` and `app/src/workspace/live_agents.rs`. Every 15 s the app
  may publish this machine's CLI agents to `agents-live:<host>` (last write wins; republished on
  change and at least once a minute; readers drop a snapshot older than 3 minutes) and may read the
  other machines' keys back.
- **Publishing** happens only while `sync.live_feed.enabled` is on: off by default, kept on for an
  install that was already signed in when the setting arrived. It does not depend on the panel
  being visible, because other readers (Iris) read the key. What goes out is status only (project,
  agent, status, tool, session id, times, Remote Control URL, background task count, host,
  platform, `machine_id`) unless `sync.live_feed.share_prompt_text` is on, which adds short capped
  prompt and reply snippets. Every text field passes `redact_secrets` either way (key, token,
  password and webhook patterns; `MachineAgents::for_publish`). With the feed off, or on sign-out,
  the app deletes this machine's key; the server drops keys not written for 24 hours.
- **Reading** other machines happens only while an Agents panel is on screen in some window (or a
  window just asked for a fresh list). The state is per admin profile, never shared with a team.
- A `LiveAgent` carries optional `remote_url` (the session's claude.ai Remote Control URL, which
  the `mnemos-agents` plugin reads from the transcript) and `background_tasks`, left out when empty
  so older clients interoperate. `remote_url` is validated in the plugin, at event parse, when
  mapping remote rows and before opening: exactly `https://claude.ai/code/session_` plus
  `[A-Za-z0-9_-]{1,128}` (`mnemos_ide_agents::is_remote_control_url`).
- `background_tasks` is nonzero while subagents, background shells or monitors are still running
  after the turn ended; the session then stays WORKING as "N background tasks".
  `ide/claude-plugin/tests/background-tasks.sh` checks the transcript walk against fixtures.

### Deleting synced data
Settings > Sync offers "Delete synced data" (`DELETE /api/tcb/me/data`) and "Delete account"
(`DELETE /api/tcb/me`), wired in `AuthManager::delete_synced_data`.
- The machine signs out of sync first (the run stops, the transcript worker included, the live
  feed key is deleted, the login leaves the keyring) and the request waits for the stop, so nothing
  from this machine lands during or after the deletion. A failed deletion keeps the client for a
  retry and never restarts sync. A 403 or 404 means nothing was deleted, and sync signs back in.
  A successful one forgets the recovery key saved here.
- Server side (Admin repo, `tcb_account.rs`): the data deletion removes every state row of the
  account, its blob ledger and blobs, and the team state of teams the caller created that nobody
  else is in. The account deletion also removes the profile and its memberships, deletes teams the
  caller created that have no other members, and keeps teams others use. Both refuse accounts not
  created from Mnemos (staff accounts) with 403 before touching anything.
- `GET /api/tcb/me` says whether the server accepts this account (`canDeleteData`); Settings > Sync
  shows an explanation instead of the buttons when it does not. A server without that route gets
  the buttons.
- Other machines still signed in to the account upload theirs again on their next sync, so the
  confirmation says to sign them out first.

### Suggest
The agents panel's Suggest runs locally: the app spawns the user's own `claude` CLI
(`claude -p`, prompt on stdin; `app/src/workspace/view/agent_suggest.rs`) with the session's
project name, last prompt and last reply, built and cleaned up by `mnemos_ide_sessions::suggest`.
Nothing goes to the sync server. Builds from before this change send that text to the server.

---

## CLI agent events

The agents panel learns about running agents from OSC 777 `mnemos://cli-agent` sequences that the
agents' hooks print into the pane (`app/src/terminal/cli_agent_sessions/`).
- Each terminal exports a random token (`MNEMOS_CLI_AGENT_TOKEN`); an event carrying it is always
  accepted. A bare sentinel without it, or upstream's `warp://cli-agent`, could be printed by
  anything in the pane, so it is dropped unless the user turns on
  `agents.third_party.accept_untokened_cli_agent_events`, and even then trusted only while the pane
  runs that agent. One exception keeps existing installs working: a bare Claude Code event is
  accepted, while the pane runs `claude`, when the installed `mnemos-agents` is older than 1.2.0
  or records no version (`event::installed_plugin_predates_token`, reading `installed_plugins.json`,
  cached 30 s). Once the plugin is updated, bare events are dropped again. The Gemini and OpenCode
  extensions still send bare events and need the setting.
- A file transport runs beside OSC (`file_ingest.rs`), because hooks on Windows have no handle to
  the ConPTY: each terminal exports its own NDJSON file (`MNEMOS_CLI_AGENT_EVENT_FILE`) in a
  directory created with mode 0700, files 0600, and a watcher feeds new lines into the same parser.
  A terminal that already got an event over OSC ignores its file copies.
- The subshell and SSH bootstrap hooks (`InitSubshell`, `InitSsh`) make Mnemos type more script
  into the PTY, so they are honoured only when they echo the one-time nonce of the script Mnemos
  wrote (`terminal_model.rs`).

---

## Updates

`app/src/autoupdate/`, with the format and keys in `crates/channel_versions` (`signed.rs`).
`RELEASING.md` covers the release side.
- Only official releases have an updater: `app/build.rs` emits an `autoupdate_config` only with
  `MNEMOS_OFFICIAL_RELEASE=1` (set by the release scripts) or `RELEASES_BASE_URL`, and only
  together with the public keys from `script/update-signing.pub` (or `MNEMOS_UPDATE_PUBLIC_KEYS`);
  it panics rather than build an updater without a key. `MNEMOS_UPDATER=off` turns it off at build
  or run time.
- The check is an anonymous GET of `{releases_base_url}/channel_versions.json`
  (`channel_versions.rs`), a minute after the first frame, then every 10 minutes and on activation.
- `fetch_version` verifies the manifest's envelopes (`signed`, then `signed_next`, through
  `signed::verify_manifest`; Ed25519 against the baked-in keys, highest known format wins) and
  takes the version, rollback flag, this platform's asset digests (`verified_assets`) and the
  display hints `update_by`, `soft_cutoff` and `last_prominent_update` from the payload
  (`PlatformRelease::apply_to`). A hint the payload leaves out is cleared, never taken from the
  unsigned legacy fields.
  Every platform checks the downloaded file's sha256 and size before mounting, copying or running
  it; Windows checks again just before it runs the installer. A manifest without `signed` is
  `UnsignedManifest` (logged, not reported) and no update; a bad signature or unknown format is an
  error and no update; a payload without this platform is no update.
- The legacy `dev` / `preview` / `stable` fields stay in the manifest for builds from before
  signing, which read only `stable.version`.

---

## Where Mnemos IDE hooks into the app

- **Boot:** `app/src/bin/mnemos.rs` applies the dev isolation defaults (debug builds that are not
  release bundles) and calls `app/src/lib.rs::run()`. `app/src/auth/auth_manager.rs` starts
  `SyncManager` at launch from the login saved in the keyring (`AdminAuth::load_from_keyring`) or
  after a new sign-in.
- **Panels:** wired in `app/src/workspace/view/left_panel.rs` (the agents panel, sessions
  included, through `view/ide_sessions.rs`) and the Drive panel (`app/src/drive/index.rs`, the
  `~/.claude` sync rows).
- **Settings > Sync:** `app/src/settings_view/sync_page.rs` (projects, `.env`, hooks sync, pending
  approvals, encryption in `sync_page/e2e.rs`) and `sync_privacy.rs` (live feed, team, deletion).
- **Tabs:** `app/src/workspace/ide_sync.rs` and `view/ide_tab_sync.rs` run the project reconcile.
- **Git tree:** the code-review header
  (`app/src/code_review/code_review_header/header_revamp.rs`).
- **Terminal:** stays upstream's (`crates/mnemos_terminal`, `app/src/terminal/local_tty/`). Claude
  runs in ordinary terminal tabs; the agents panel's sessions list and agent restore resume
  sessions there.
- **Settings:** upstream's `crates/settings`. Secrets never go in settings: the sync token goes
  through `keyring`, provider keys, MCP tokens and the sync recovery key through upstream
  `secure_storage`.

---

## Upstream code that is still compiled in

Much of Warp's cloud product is deleted or compiled out (`docs/upstream-diff.md` has the list):
billing, server experiments, Firebase, voice input, computer use, the wasm build, the plugin host,
bundled skills, block sharing (now a local "Copy as Markdown"), the antivirus scan and the
staging-server code are gone. Behind non-default cargo features, so absent from every bundle:
`builtin_agent` (the built-in Oz agent client, BYO provider keys and Bedrock credentials),
`passive_suggestions`, `telemetry` and `crash_reporting`.

Upstream auth, cloud objects and GraphQL, Drive, teams, referrals and the AI model list are still
compiled in. What keeps them inert:

- The baked channel config in `app/build.rs` points `server_root_url` at the Mnemos sync server,
  which has no upstream `/graphql`, and sets `telemetry_config` and `crash_reporting_config` to
  `null`. RudderStack telemetry also needs the `telemetry` feature
  (`ChannelState::is_telemetry_available`).
- The teams, referrals and other cloud settings pages are not in the sidebar, and
  `SettingsSection::is_hidden` keeps every other route from opening them
  (`app/src/settings_view/mod.rs`).
- Under the sync sign-in, the GraphQL gate and `has_upstream_server_session` described in
  "Sign-in" above; the cloud-object update websocket also opens only with an upstream session
  (`Listener::start_listener`). Upstream's plain HTTP calls (the `/api/v1/` REST API,
  `/client/login`, `/ai/*`, `/current_time`) and a few directly-sent GraphQL operations (`GetUser`,
  `FreeAvailableModels`) are not covered by either gate and still go to `server_root_url` when
  they run.
- `Credentials::Firebase` remains as a persisted-user format; a Firebase token is refused locally.

---

## Build, test and CI

- **Dev isolation.** A debug build that is not a `release_bundle` isolates itself:
  `app/src/bin/mnemos.rs` calls `mnemos_ide_core::paths::apply_dev_isolation_defaults` first,
  which sets `MNEMOS_DATA_PROFILE=dev` when it is unset and, when no data dir is set,
  `MNEMOS_DATA_DIR=<data dir>/Mnemos-<profile>`. `./script/run` sets the same defaults itself.
  `MNEMOS_DATA_PROFILE=` (empty) opts out. On macOS the profile also moves the config dir (and
  `settings.toml`) to `~/.mnemos-<profile>`. Details: "How to build & run" in `AGENTS.md`.
- **Test isolation.** With `mnemos_core`'s `test-util` feature (dev-dependencies), while no binary
  has set a channel, `config_local_dir()` and `data_dir()` point into a per-process temp dir
  (`ChannelState::unit_test_home_dir`), so unit tests never touch the real `~/.mnemos-local`.
- Type-check: `cargo check -p mnemos --features gui --lib --tests`.
- Tests: `cargo test -p <mnemos-ide crate>` for one crate, or `script/ide-check` for the tests and
  clippy of all of them plus the leak check and cargo-deny.
- **CI** (`.github/workflows/`):
  - `pr.yml`, on pull requests and pushes to `main`: text checks (`script/oss/grep-checks.sh`), the
    leak check, the plugin tests and cargo-deny on Linux; `cargo check` and `script/ide-check` with
    a scratch `HOME` on macOS, Windows and Linux (the Linux leg is advisory).
  - `dco.yml`: every commit of a pull request needs a `Signed-off-by:` trailer, checked by the base
    commit's copy of `script/oss/check-dco.sh`, never the pull request's.
  - `release.yml`: builds, uploads and publishes a release from a signed version tag; see
    `RELEASING.md`.
- Releases: `RELEASING.md`.

## Migration from the v1 Electron app

There is no migrate command. On first run `ProjectStore` seeds `store/projects.json` once from
the v1 store (see `mnemos-ide-projects`), without modifying it. Existing `.twincurrent-build/`
directories stay in place and nothing reads them. Claude memory under `~/.claude/projects/` is
untouched and syncs as described above.
