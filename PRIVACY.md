# Privacy

What Mnemos IDE keeps on your machine, what it sends, and to whom. This describes the code in
this repository. Where the code has a known limit, this file says so. The hosted sync service is
run by TwinCurrent ApS; see "Contact" at the end.

## Summary

- Without signing in, nothing syncs. The only network request Mnemos itself makes on its own is
  the update check, which is anonymous.
- No telemetry and no crash reporting in released builds.
- Sync is optional. When you sign in, the data listed under "What sync uploads" goes to the sync
  server. It is readable by whoever runs the server unless you turn on end-to-end encryption,
  and some of it stays readable even then (see "End-to-end encryption").
- Secrets in `~/.claude/.env`, in the `env` and `headers` of your MCP servers and in
  secret-named `env` entries of Claude Code's `settings.json` are not uploaded by default.
- Agent CLIs you run in the terminal (Claude Code, Codex, Gemini CLI, OpenCode and others) talk
  to their own providers under their own terms. Mnemos does not proxy that traffic.

## On your machine

Stored locally and never sent anywhere by Mnemos:

- Your files, shell history and terminal block output. Block output and command history are
  kept in the app's SQLite database for session restore. With secret redaction ("safe mode",
  `privacy.secret_redaction.enabled`, off by default) turned on, detected secrets are masked
  before a block is stored. The command "Clear Stored Terminal History" deletes the stored
  blocks and command history, and `terminal.stored_history_retention_days` (0 by default, which
  keeps them until their pane closes or you clear them) deletes older entries at startup.
- Secrets, in the OS secure storage: the sync login (service `tcbv2`, or `tcbv2-<profile>` for a
  profiled build), in the login keychain on macOS and the Windows Credential Manager on Windows;
  and the end-to-end encryption recovery key (item `MnemosSyncRecoveryKeys`), MCP OAuth tokens
  and any AI provider API keys you entered, in the login keychain on macOS and DPAPI-encrypted
  files on Windows. "Uninstalling" below lists where each one is.
- The Mnemos IDE store under the Mnemos data directory (`store/`): `projects.json`,
  `workspace_state.json` (open tabs and the per-project sync opt-out), `agent_sessions.json`,
  `sync_options.json` (the `.env` choice), `sync_gates.json` (approvals and pending changes,
  see below), `team_choice.json`, `machine_id` (a random id), `transcript_sync.json` and
  `sync_baseline.json`. These files are never uploaded as such. The machine id is sent as part
  of transcript paths and the live feed, as described below.
- Files that a sync pull replaced or removed are moved to `~/.claude/backups/tcb-sync-<date>/`.

Terminal sessions get `MNEMOS_*` environment variables plus `WARP_TERMINAL` and
`WARP_CLIENT_VERSION` mirrors, so that agent plugins written for Warp recognise the terminal.
Any program you run can read them; they carry no personal data.

## Telemetry and crash reporting

None by default. Released builds have no telemetry or crash reporting destination
(`telemetry_config` and `crash_reporting_config` are `null` in `app/build.rs`), the RudderStack
client is only compiled with the `telemetry` cargo feature, which no bundle script enables, and
stable builds do not include the crash reporter. The two switches in Settings > Privacy are off
by default.

## Update check

Released builds check for updates when the app is activated, and on a timer that starts 60
seconds after the first window has drawn and repeats every 10 minutes. The check is a plain `GET` of the static release manifest
`https://admin.twincurrent.dk/releases/channel_versions.json`. It carries no install id, no login
token and no account information. Because the manifest is served from the sync server's host,
the request does carry the headers Mnemos adds for that host: the app version
(`X-Mnemos-Client-Version`), the OS name and version (`X-Mnemos-OS-*`) and the client kind
(`X-Mnemos-Client-ID`). The server sees your IP address, as with any HTTP request.

The changelog is read from the same manifest, only when a version is launched for the first time
or when you open the changelog. A downloaded update is installed only after the manifest's
signature and the file's SHA-256 check out (see `SECURITY.md`).

Builds from source have no updater and make no update or changelog request.

## Upstream Warp features

Mnemos is a fork of Warp. Warp's cloud features would talk to `server_root_url`, which in Mnemos
is the sync server, not Warp's. What that means today:

- The built-in Warp agent (agent mode requests, with any provider API keys and AWS Bedrock
  credentials) is only compiled with the `builtin_agent` feature, which released builds do not
  enable. Without it, agent requests fail locally and nothing is sent.
- Passive prompt and code suggestions are only compiled with the `passive_suggestions` feature,
  which released builds do not enable.
- Voice input is removed. Block sharing is replaced by "Copy as Markdown", which copies to your
  clipboard and uploads nothing. Firebase sign-in is removed.
- Under the Mnemos sign-in, Warp's GraphQL calls stop before any network request, and the cloud
  object, workspace metadata and model list pollers do not run. A few plain HTTP calls from
  upstream code can still reach `server_root_url`; `docs/upstream-diff.md` lists them.

## Suggest (agents panel)

"Suggest" runs on your machine. It hands the agent session's project name, the last prompt (at
most 2,000 characters) and the end of the last reply (at most 6,000 characters) to your own
`claude` CLI (`claude -p`, with no tools, MCP servers, hooks or session saving). The model call
is made by that CLI under your own Claude login. Nothing goes to the Mnemos server, and nothing
is stored or synced.

Builds from before this change sent that text to the sync server, which asked Google Gemini
(on TwinCurrent's account) for the suggestion. The server keeps that endpoint for those builds
until they are updated.

## Signing in

Sign-in happens in your browser on the sync server's site. The app sends a PKCE challenge (the
SHA-256 of a random verifier) and a random `state` value in the URL. The browser hands the app a
one-time code (valid for 120 seconds, single use), and the app trades it, with the verifier, for
the login tokens over HTTPS (`POST /api/auth/desktop-exchange`). Tokens never appear in a URL.
The verifier stays in the app's memory. The tokens are stored in the `tcbv2` keychain entry
described above. Servers from before this change still put the tokens in the redirect URL, and
the app accepts that redirect until 2027-01-31.

## What sync uploads

Sync needs an account on the sync server. Today the only server is the one TwinCurrent runs.
Every sync request carries your login token and an `X-Mnemos-Sync-Protocol` header with the
client's protocol number (currently 1).

What is uploaded, and pulled onto your other signed-in machines, by sync key:

| Data | Sync key | Notes |
| --- | --- | --- |
| Projects: name, git remote, the local path on each platform, and whether each is open as a tab | `projects` | Projects you turned sync off for on this machine are left out. |
| The Mnemos IDE store's `settings.json` | `settings` | |
| Mnemos `settings.toml`, the settings marked as synced only | `mnemos-user-settings-v2` | Never synced: shell overrides, the Bedrock settings, the MCP file toggle, agent execution allowlists and denylists, agent permissions, the tab config path, secret redaction, and every setting marked local. |
| Claude Code memory of each project | `claude-memory:<profile id>:<owner>/<repo>` | Skipped for projects you opted out here. |
| `~/.claude/CLAUDE.md` and `settings.local.json` | `claude-md`, `claude-settings-local` | |
| The `~/.claude` directories `agents`, `commands`, `skills`, `rules` and `output-styles` | `claude-agents`, `claude-commands`, `claude-skills`, `claude-rules`, `claude-output-styles` | Text files up to 512 KB each, of the expected types only. |
| `~/.claude/settings.json` | `claude-settings` | Per platform. Its commands (`hooks`, `statusLine`, `env`, `permissions` and similar keys) only sync with hooks sync on. Secret-named `env` entries (names containing `KEY`, `TOKEN`, `SECRET`, `PASSWORD`, `AUTH`, `COOKIE` and similar) are never uploaded. |
| `~/.claude/hooks/` and `statusline.py` | `claude-hooks`, `claude-statusline` | Only with hooks sync on (off by default, per machine). |
| `~/.claude/mcp-servers/` | `claude-mcp-servers` | Per platform. |
| The `mcpServers` of `~/.claude.json` | `claude-json-mcp-servers-v2` | Per platform, without each server's `env` and `headers`. |
| `~/.claude/.env` | `claude-env` | Only on a machine where you turned it on (Settings > Sync, off by default). |
| Claude Code session transcripts | blobs under `claude-sessions/` | See below. |
| Which sessions each machine uploaded | `claude-session-index` | Session ids, machine ids, segment numbers. |
| The live agents feed | `agents-live:<host>` | See below. |

Older builds used other keys (`mnemos-user-settings` for the whole `settings.toml`,
`claude-json-mcp-servers` for MCP servers with their `env` and `headers`, and
`mnemos-workspace-state`). This build does not write the first or the last. It removes its own
platform's entry from the old MCP key when it pushes. Values that older builds wrote stay
on the server until they are overwritten or deleted.

### Transcripts

Claude Code transcripts (`~/.claude/projects/**/<session>.jsonl`) hold your prompts, the agent's
replies, tool calls with their output, and file contents the agent read. While you are signed
in, each machine uploads the lines it wrote, as gzip segments of up to 4 MiB, plus a small
metadata file per session: the session id, the project (id, git remote, name) or code folder,
the working directory relative to it, the origin machine's project root or home path, the
platform and timestamps. Sessions untouched for 30 days are not uploaded. Sessions whose working
directory is inside `~/.claude`, the Mnemos data directory or a system temp folder are never
uploaded.

Turning sync off for a project on a machine stops its transcripts and memory from being uploaded
or pulled there. At the next start, that machine also deletes the transcripts it had already
uploaded for that project. Copies that other machines already pulled stay on them, and so do
uploads from other machines.

### Live agents feed

The live feed lets your other machines (and other apps on your account) list the agents running
on this one. It is off for new installs. An install that was already signed in to sync when this
setting arrived keeps it on, and also keeps sending prompt and reply text, until you change it
in Settings > Sync.

While it is on, every 15 seconds when something changed (and at least once a minute), the app
writes `agents-live:<host name>` with: the host name, platform, the random machine id, and for
each running agent its project name, agent kind, status, last tool name, Claude session id,
last activity time, Claude Code Remote Control URL and number of background tasks. The first
200 characters of the last prompt and reply are included only when "Include prompt and reply
text" is on. Every text field is passed through a secret filter either way (private keys,
bearer and basic credentials, `Authorization` and `Cookie` headers, common API token formats,
webhook URLs, password flags and `password=`-style pairs, long hex strings, and URLs with a
password). The filter is pattern based and can miss a secret in an unusual format.

The key is deleted when you turn the feed off and when you sign out. The server deletes a feed
that has not been written for 24 hours. Other machines' feeds are only read while an Agents
panel is on screen, or for 45 seconds after a window asks for a fresh list. The feed belongs to your account and is never shared with a team.

## Teams

Joining a team is a choice made on each machine (Settings > Sync); being a member on the server
is not enough. A machine that was already signed in before this choice existed keeps the team it
used. A team owner's machine copies its `CLAUDE.md`, agents, commands, skills, rules and hooks to
the team. Members pull them (all but `CLAUDE.md`, which is never written to a member's disk).

Team data is stored on the server without end-to-end encryption and is readable by whoever runs
the server, whether or not you turn encryption on.

## End-to-end encryption

Optional, off by default, turned on per account in Settings > Sync. The first machine generates a
random 256-bit key and shows it once as a recovery key; your other machines need it typed in. The
key stays in each machine's secure storage and is never sent. Values are sealed with
XChaCha20-Poly1305.

When it is on, the server stores ciphertext for every personal sync key and every transcript
blob. It can still see:

- key names, which include your profile id and, for memory, each project's `<owner>/<repo>`;
- blob paths, which include session ids, machine ids and segment numbers;
- the size, version and time of every write;
- the `e2e-meta` marker: a key id, a sealed check value and its creation time;
- the live agents feed (`agents-live:*`), which stays readable so other apps can read it;
- all team data.

When you turn it on, existing plaintext on the server is encrypted and then deleted during a
24-hour migration window. Server backups and blob storage retention may still hold the older
plaintext (see "Retention"). The server cannot recover a lost recovery key. Server features that
read your `projects` key (project context for TwinCurrent's own tools) lose that data for an
encrypted account.

If the marker disappears from the server, machines that have the key pause sync instead of
continuing unencrypted. "Start over" in Settings > Sync forgets the key on that machine and syncs
unencrypted from then on.

## Secrets

- `~/.claude/.env` syncs only on machines where you turned it on. A copy uploaded earlier (by an
  older build, or before you turned it off) stays on the server until you remove it with "Remove
  .env from the sync server" in Settings > Sync. Older builds still signed in to the account keep
  syncing `.env`.
- The `env` and `headers` of MCP server definitions are never uploaded.
- Secret-named `env` entries in Claude Code's `settings.json` are never uploaded. Values that
  older builds already put on the server are kept there for them; your local values always win.

## Synced content that runs code

Some synced files run code on the machine that receives them: hooks, the command keys of Claude
Code's `settings.json`, `statusline.py`, MCP server definitions and scripts, and agent, command
and skill files. A change to any of these that arrives from the server or a team is only written
on a machine after you approve it there (by content hash, in Settings > Sync). Approvals and
pending changes stay on that machine. Hooks and `statusline.py` do not sync at all unless you turn
on hooks sync on that machine. Rules and `CLAUDE.md` are prompt content and are not gated.

## Deleting your data

Settings > Sync offers two actions while you are signed in:

- **Delete synced data** deletes every sync key of your account (projects, settings, Claude
  config, memory, the session index and the live feed), every transcript blob, your storage usage
  records, and the team data of any team you created that has no other members. Your account,
  your team memberships and the files on your machines stay.
- **Delete account** does the same, then deletes your account with its sessions, notifications
  and settings on the server. You are removed from every team. Teams you created with no other
  members are deleted. Teams you created that others use are kept for them, with the longest
  standing member as creator; their team data stays.

Limits:

- The machine you delete from signs out of sync first. Other machines still signed in upload
  their copy again on their next sync, so sign them out first.
- Accounts that were not created from Mnemos (TwinCurrent staff accounts on the admin platform)
  cannot use either action; the app says so instead of offering them. Their data is deleted by
  an operator on request (see "Contact").
- Neither action reaches copies on your machines, server backups, or logs.
- After deleting, the recovery key saved on that machine is forgotten.

Signing out does not delete synced data. It only deletes this machine's live feed key.

## Retention

- Synced data stays on the server until it is overwritten or deleted as described above. The live
  feed is the exception: it expires after 24 hours without an update.
- Sign-in codes are deleted when they are exchanged, and expired ones on a later sign-in.
- How long server backups and logs are kept is not yet documented here. Ask at
  mnemos@twincurrent.dk.
- Local data stays until you delete it. Removing the app leaves most of it in place; see
  "Uninstalling".

## Uninstalling

Removing Mnemos from a machine does not touch the sync server. To remove your data there too, use
"Delete synced data" or "Delete account" (see "Deleting your data") before you uninstall, while
you are still signed in. Otherwise sign out of sync first, so the machine's live feed entry is
removed.

Quit Mnemos, then remove what you want gone. Paths are for an installed release build (bundle id
`dev.mnemos.Mnemos`).

**macOS**

- The app: `/Applications/Mnemos.app`.
- The Mnemos IDE store (projects, tabs, agent sessions, sync options, approvals, machine id):
  `~/Library/Application Support/Mnemos/`.
- The app's state and SQLite database (windows, panes, block output, command history):
  `~/Library/Group Containers/2BBY89MBSN.dev.mnemos/Library/Application Support/dev.mnemos.Mnemos/`
  in a signed build, otherwise `~/Library/Application Support/dev.mnemos.Mnemos/`.
- Settings, themes and workflows: `~/.mnemos/`. Other app preferences:
  `defaults delete dev.mnemos.Mnemos`.
- Logs: `~/Library/Logs/mnemos.log` and its rotated backups next to it.
- Keychain items, in the login keychain (Keychain Access, or `security`):
  - the sync login: `security delete-generic-password -s tcbv2 -a admin-token`;
  - the recovery key: `security delete-generic-password -s dev.mnemos.Mnemos -a MnemosSyncRecoveryKeys`;
  - MCP OAuth tokens and AI provider keys: further items with service `dev.mnemos.Mnemos`
    (`security delete-generic-password -s dev.mnemos.Mnemos` removes one at a time; repeat until
    it reports none).

**Windows**

- The app: uninstall "Mnemos" from Settings > Apps (by default installed per user under
  `%LOCALAPPDATA%\Programs\Mnemos`). The uninstaller also removes `%APPDATA%\mnemos\Mnemos\` and
  `%LOCALAPPDATA%\mnemos\Mnemos\` (settings, the SQLite database, logs, and the DPAPI-encrypted
  secret files, the recovery key `dev.mnemos.Mnemos-MnemosSyncRecoveryKeys` among them), the
  registry key `HKCU\Software\Mnemos.dev\Mnemos`, the start-at-login entry and the Explorer menu
  entries.
- Left behind: the Mnemos IDE store, `%APPDATA%\Mnemos\store\` (delete `%APPDATA%\Mnemos\` once the
  uninstaller has run), and `%USERPROFILE%\.mnemos\` if it exists.
- The sync login, in the Windows Credential Manager (Windows Credentials, generic credential
  `admin-token.tcbv2`): `cmdkey /delete:admin-token.tcbv2`.

**Dev builds and profiles.** A build run with a data profile (`./script/run` uses `dev`) keeps its
own copies next to the ones above, named with the profile: the store under `Mnemos-<profile>`,
`dev.mnemos.Mnemos-<profile>` (macOS app state and keychain service; on Windows
`%APPDATA%\mnemos\Mnemos-<profile>` and `%LOCALAPPDATA%\mnemos\Mnemos-<profile>`),
`~/.mnemos-<profile>`, `mnemos-<profile>.log`, and the sync login under `tcbv2-<profile>`
(`tcbv2-isolated` for a build with a redirected data directory and no profile). A build pointed
elsewhere with `MNEMOS_DATA_DIR` keeps its store there.

**Linux** (untested): `~/.local/share/Mnemos/` and the app's directories under `~/.local/share`
and `~/.config`, plus `~/.mnemos/`.

**On every OS**, these are not Mnemos's own files and stay unless you remove them:

- The Claude Code plugin: `claude plugin uninstall mnemos-agents@mnemos`, then
  `claude plugin marketplace remove mnemos`. If `~/.claude/settings.json` still lists
  `mnemos-agents@mnemos` under `enabledPlugins`, delete that entry.
- Files that sync pulls replaced or removed: `~/.claude/backups/tcb-sync-<date>/`.
- What sync wrote into `~/.claude` (config, memory, other machines' session transcripts under
  `~/.claude/projects/`) belongs to Claude Code now and stays with it.

## Where the data is

- The sync server runs on Railway. Its PostgreSQL database is on Railway (connections require
  TLS) and transcript blobs are in Cloudflare R2. The browser sign-in pages are served by Vercel.
- Only the two TwinCurrent founders can read the production database and the bucket.

## Dev builds and other servers

- Debug builds started with `./script/run` use their own data profile: their own data directory,
  their own keychain entry and their own machine id, so they do not share a login or local state
  with the installed app.
- A build from source can be pointed at another sync server with `MNEMOS_SERVER_ROOT_URL`
  (for example a local one). Released builds ignore it. A saved login only works against the
  server it was made for. Such a build sends its sync data to that server, under that server's
  operator's terms.
- The server is not open source yet. A server you can run yourself is planned.

## Contact

TwinCurrent ApS, Denmark (CVR 45696286). Privacy questions and deletion requests:
**mnemos@twincurrent.dk**.
