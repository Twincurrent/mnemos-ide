# Setting up a second machine

Sync lets a second computer (a Mac and a Windows PC, say) share your projects, your Claude Code
config and memory, and your Claude Code sessions, so `claude --resume` continues on one machine a
session started on the other. This page walks through it. Sync is optional: without signing in,
Mnemos works fully on one machine and nothing is uploaded.

What sync uploads, field by field, is in [`PRIVACY.md`](../PRIVACY.md) ("What sync uploads"). Read
the README's ["Sync: honest status"](../README.md#sync-honest-status) first: the only sync server
today is the one TwinCurrent runs, its source is not public, and without end-to-end encryption it
can read what you sync.

## Before you start

- Run the same Mnemos version on both machines. An older build cannot read settings or encrypted
  data that a newer one wrote.
- Install [Claude Code](https://docs.claude.com/en/docs/claude-code) and the `mnemos-agents`
  plugin on each machine (README, "Try it on one machine", step 1). Plugins are not synced:
  `~/.claude/plugins` stays on each machine, and the `plugins` key of Claude Code's
  `settings.json` only syncs with hooks sync on (below).
- If the second machine already has a `~/.claude` of its own, back it up. Sync merges the
  `~/.claude` directories of your machines, so files from both end up on both. A local file that a
  pull replaces or removes is moved to `~/.claude/backups/tcb-sync-<date>/` first.

## 1. Sign in on the first machine

1. Open Settings > Sync and click **Sign in**.
2. Your browser opens the sync server's sign-in page. Sign in there; the browser then hands the
   login back to Mnemos through a `mnemos://` link. Allow the browser to open Mnemos when it asks.
3. Settings > Sync now shows that you are signed in. Mnemos saves the login in the OS keychain
   (macOS) or the Windows Credential Manager, under the service `tcbv2`.

If the login later expires or goes missing, a "Sync signed out" chip appears in the tab bar; click
it to sign in again.

## 2. Optional: turn on end-to-end encryption

Do this before the second machine signs in, so its first upload is already encrypted.

1. Make sure every machine that will sync this account runs a build with encryption support. An
   older build cannot read encrypted data and stops receiving changes.
2. Settings > Sync > Security: **Turn on**, then confirm.
3. Mnemos shows your recovery key **once**. Copy it into a password manager, then click
   **I saved it**. Nobody, the server included, can recover it for you.

The key is saved in this machine's secure storage (item `MnemosSyncRecoveryKeys`: the login
keychain on macOS, a DPAPI-encrypted file on Windows) and is never sent. Data already on the
server is encrypted and the plaintext deleted during the next 24 hours. What stays readable to the
server even then is listed in `PRIVACY.md` ("End-to-end encryption"): key names, sizes, times,
team data and the live agents feed.

## 3. Sign in on the second machine

1. Install Mnemos and the plugin, then open Settings > Sync and click **Sign in**, with the same
   account as on the first machine.
2. If you turned on encryption, Settings > Sync shows **Enter your recovery key** and personal
   sync stays paused until you do. Paste the key and click **Unlock**.
3. The first pull brings your projects, Claude Code config, memory and sessions. Give it a moment;
   transcripts are fetched in the background.

## What happens next

- **Projects and tabs.** Projects that are open as tabs on one machine open as tabs on the other.
  Each machine finds its own checkout of a project by git remote, or by folder name under the
  usual code folders (`~/code`, `~/dev`, `~/projects`, `~/src`, `~/repos` and similar, plus
  `C:\code`, `C:\dev` and the like on Windows). Clone a repository into one of those folders and
  the next check (within about five minutes) links it. A project with no checkout yet opens in
  your default directory, and Settings > Sync lists it as "Not on this machine". Closing a tab
  hides the project on every machine; it never deletes it.
- **Sessions.** The agents panel's SESSIONS list shows the other machine's Claude Code sessions
  once their transcripts are pulled. Click one to resume it here; right-click to fork it. A session
  still running on the other machine is never resumed in place: fork it, or use its `open` button
  (below).
- **Config and memory.** `CLAUDE.md`, agents, commands, skills, rules, output styles, MCP server
  definitions (without their `env` and `headers`) and each project's Claude Code memory follow
  you. Settings marked as synced in Mnemos's `settings.toml` do too.
- **Secrets stay put.** `~/.claude/.env` syncs only on machines where you turn on **Sync
  ~/.claude/.env** (off by default, per machine). Secret-named `env` entries in Claude Code's
  `settings.json` never leave the machine.

## Approving code from your other machine

Some synced files run code: hooks, `statusline.py`, the command keys of Claude Code's
`settings.json` (`hooks`, `statusLine`, `env`, `apiKeyHelper`, `plugins` and similar), MCP server
definitions and scripts, and agent, command and skill files. A change to any of these that
arrives from another machine is not written until you approve it on this one.

- Settings > Sync > "Code from your other machines" lists each waiting change as **new** or
  **changed**, with a diff. **Approve** writes it, **Approve all** writes every one, and
  **Dismiss** keeps this machine's copy and sends it back up.
- **Sync hooks and Claude Code commands** is off by default on each machine. While it is off,
  hooks, `statusline.py` and those `settings.json` command keys do not sync at all. Turn it on per
  machine if you want them to follow you; every incoming change still waits for approval.
- Approvals are kept on each machine (`store/sync_gates.json`); approving on one machine does not
  approve on the other.

## The live agents feed

Settings > Sync > "Live agents feed" > **Share running agents with your other machines** is off
for new installs. Turn it on, on each machine whose agents you want to see elsewhere. While it is
on, every 15 seconds the machine publishes its running agents (project, agent, status, last tool,
session id), so the agents panel on your other machines lists them. A running Claude session with
Remote Control on gets an `open` button there, which opens its claude.ai Remote Control page.

**Include prompt and reply text** adds the first 200 characters of each agent's last prompt and
reply. It is a second opt-in, and secret-looking text is redacted either way. The feed is not
end-to-end encrypted. It is removed from the server when you turn it off or sign out.

## Keeping a project on one machine

Settings > Sync lists your projects with a checkbox each. New projects sync by default. Uncheck a
project to keep it local on this machine: its registry entry, Claude Code memory and transcripts
are then neither uploaded nor pulled here, and transcripts this machine had already uploaded for
it are deleted from the server at the next start. Copies other machines already pulled stay on
them.

## Stopping

- **Sign out of sync** (on the main Settings page, or in the workspace menu) stops that machine
  syncing. It deletes that machine's live feed entry and nothing else on the server; your tabs
  and local files stay.
- **Delete synced data** or **Delete account** in Settings > Sync removes your data from the
  server. Sign out on your other machines first, or they upload their copy again. Details are in
  `PRIVACY.md` ("Deleting your data").
- To remove Mnemos from a machine entirely, see `PRIVACY.md` ("Uninstalling").
