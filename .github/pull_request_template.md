## What and why

<!-- What does this change, and why? Link the issue it fixes: "Fixes #123". -->

## How it was tested

<!-- Commands you ran and what you checked by hand. Which platform(s)? -->

- [ ] `cargo check -p mnemos --features gui --lib --tests`
- [ ] `script/ide-check` (tests and clippy for the `ide/` crates, grep checks, leak check, cargo-deny), ideally with a scratch `HOME` (CONTRIBUTING.md)
- [ ] Tested on: macOS / Windows / Linux

## Checklist

- [ ] Edits to upstream code under `app/` or `crates/` are marked with a `Mnemos IDE:` comment
- [ ] Docs (`README.md`, `ARCHITECTURE.md`, `AGENTS.md`, `PRIVACY.md`) updated where behaviour changed
- [ ] Every commit is signed off (`git commit -s`; the DCO check fails otherwise, see CONTRIBUTING.md)
- [ ] No secrets, personal paths or private data in the diff
