# Security Policy

## Reporting a vulnerability

Please report vulnerabilities **privately**. Do not open a public issue, pull request or
discussion for a security problem. Either:

- use GitHub's private vulnerability reporting: open the repository's **Security** tab and
  choose **Report a vulnerability**; or
- email **mnemos@twincurrent.dk** with "Security" in the subject. Use this if the button is
  missing or you prefer email.

Include what you can of:

- the affected version (Settings > About, or the release tag) and platform;
- the component (for example sync, a URL handler, the terminal parser, the updater);
- steps to reproduce, or a proof of concept;
- the impact as you understand it.

Mnemos IDE is maintained by a team of two. We aim to acknowledge a report within a week, keep
you updated while we work on it, and credit you in the advisory when it is published, unless you
prefer not to be named. Please give us a reasonable time to ship a fix before disclosing details
publicly; we will agree on a date with you.

## Supported versions

Only the latest stable release receives security fixes. Released builds update themselves, so a
fix ships as a new release. Builds from source do not update themselves.

## Scope

In scope:

- The code in this repository: the Mnemos IDE crates under `ide/`, the Claude Code plugin in
  `ide/claude-plugin`, and our changes to the upstream Warp code under `app/` and `crates/`.
- Released Mnemos builds and installers, the release scripts and workflows, and the update path.
- How the client talks to the sync and update server: sign-in, token handling, what it uploads,
  and what it does with what it pulls.

Out of scope, please report elsewhere:

- Vulnerabilities in upstream Warp that also affect Warp itself: report them to Warp. Tell us as
  well if Mnemos ships the affected code.
- Third-party agent CLIs (Claude Code, Codex, Gemini CLI, OpenCode and others) and other
  dependencies: report them to their maintainers. We still want to hear about it if Mnemos makes
  a problem in one of them exploitable.
- Findings that need an already-compromised machine or account, and social engineering.

Please do not test against other people's accounts or data, and do not run load or denial of
service tests against the hosted server.

## Security facts

What the code does today. `PRIVACY.md` describes what data goes where.

### Transport and sign-in

- Released builds talk to the sync and update server at `https://admin.twincurrent.dk` over
  HTTPS. Only builds from source can be pointed at another server (`MNEMOS_SERVER_ROOT_URL`),
  and a saved login is only used against the server it was made for.
- Sign-in uses PKCE (S256). The browser redirect to the app (`mnemos://`) carries a one-time
  code (valid 120 seconds, single use) and a `state` value, never a token; the app exchanges the
  code with its in-memory verifier over HTTPS. The server only redirects to the `mnemos` scheme.
  An access token the app cannot decode fails the sign-in. Servers from before PKCE still send
  tokens in the redirect, which the app accepts until 2027-01-31.
- The sync login is kept in the OS keychain (service `tcbv2`, `tcbv2-<profile>` for profiled
  builds). Other secrets use the OS secure storage (login keychain on macOS, DPAPI on Windows).

### Updates

- The release manifest carries an Ed25519 signature over a payload that names, per platform, the
  version and the SHA-256 and size of each asset. Clients verify the signature against public
  keys compiled into the build, and refuse a manifest that is unsigned, signed by an unknown key
  or badly signed.
- A downloaded asset is installed only if its SHA-256 and size match the signed payload. On
  Windows the installer is checked again just before it runs.
- The release scripts sign only assets whose hash a build of that release vouched for, and keep
  the signing key out of the build's environment.

### End-to-end encryption

- Optional. When on, personal sync data and transcripts are sealed with XChaCha20-Poly1305 under
  a random 256-bit key, shown once as a recovery key and kept only in each machine's secure
  storage (item `MnemosSyncRecoveryKeys`). The server cannot recover a lost key.
- Team data and the live agents feed are not end-to-end encrypted. Key names, blob paths, sizes,
  versions and times stay visible to the server (see `PRIVACY.md`).
- Each value is bound to its key name or blob path, and state values carry a sequence number,
  so a server cannot move a value to another key or, within one run of the app, replay an older
  copy than one already seen (up to a 5-minute tolerance for clock skew).
- While encryption is on, plaintext found under a personal key is only taken during the 24-hour
  migration window sealed in the marker; after it, plaintext is ignored. The plaintext copy is
  deleted with a conditional delete, so a concurrent write from an older build is not lost.
  Transcript blobs have no versions and are not covered by that check; they are written once.
- If the encryption marker disappears from the server, machines that hold the key pause sync
  instead of uploading unencrypted data, until the marker returns or the user chooses "Start
  over" on that machine.

### Synced content that runs code

- Hooks, the command keys of Claude Code's `settings.json` (`hooks`, `statusLine`,
  `apiKeyHelper`, `env`, `permissions` and others), `statusline.py`, MCP server entries and
  scripts, and agent, command and skill files are gated. A change pulled from the account or a
  team, deletions included, is written only after the user approves its content hash on that
  machine. Until then the local copy stays and a push sends the server's copy, never undoing the
  other side.
- Hooks and `statusline.py` do not sync at all unless hooks sync is turned on per machine (off by
  default). Team hooks follow the same opt-in and approval.
- Approvals and pending changes are stored only on the machine (`store/sync_gates.json`).
- Rules and `CLAUDE.md` are prompt content and are not gated.
- Mnemos `settings.toml` syncs only settings marked as synced. Settings that name a program to
  run, widen what the agent may execute, or lower a protection (shell overrides, agent command
  allowlists and denylists, secret redaction, Bedrock settings and others) never sync.

### Secrets in sync

- `~/.claude/.env` syncs only where the user turned it on. MCP server `env` and `headers`, and
  secret-named `env` entries of Claude Code's `settings.json`, are never uploaded.
- The live agents feed passes every text field through a pattern-based secret filter (private
  keys, bearer and basic credentials, `Authorization` and `Cookie` headers, common API token
  formats, webhook URLs, password flags and `password=`-style pairs, long hex strings, URLs with a
  password). Prompt and reply text is only included when the user opts in. A pattern filter can
  miss secrets in unusual formats.

### Terminal and agent events

- Agent status events written into the terminal (OSC sequences) must carry a per-terminal token
  that Mnemos puts in the shell's environment. Events without it are dropped unless the user
  turns on `agents.third_party.accept_untokened_cli_agent_events`. One exception keeps older
  installs working: on a machine whose installed `mnemos-agents` plugin is older than 1.2.0 (or
  records no version), untokened Claude events are accepted while the pane is running `claude`.
- The subshell and SSH detection hooks (`InitSubshell`, `InitSsh`) are honoured only when they
  echo a one-time nonce, so program output cannot make Mnemos type a script into the terminal.
- Language servers and Node.js that Mnemos downloads are pinned to versions with SHA-256 digests
  and verified before use.
- The `ide/mnemos-ide-*` crates forbid `unsafe` code.

### Deletion

- "Delete synced data" and "Delete account" in Settings > Sync run as one server transaction
  under the account's write lock. The deleting machine stops sync, including its transcript
  uploads, before the request is sent. Other signed-in machines can upload again until they are
  signed out. Staff accounts of the admin platform are refused by the server and are not offered
  the actions.

### Facts only the operators can confirm

- The sync server runs on Railway, its PostgreSQL database is on Railway with TLS required, and
  transcript blobs are stored in Cloudflare R2. The sign-in pages are served by Vercel.
- Production database and bucket access is limited to the two TwinCurrent founders.
- Backup and log retention periods, and the providers' at-rest encryption settings, are not yet
  documented here. Ask at mnemos@twincurrent.dk if you need them before they are.
- The update signing key is kept offline on the release maintainer's machine, outside the repo
  and outside every build environment. Rotation is described in `RELEASING.md` ("Rotation").
