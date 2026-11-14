# XkyHold-lx

Linux desktop app in the XkyHold family: a full encrypted backup of `$HOME`
(or parts of it) to the user's Google Drive or a local folder. Plan,
decisions L1–L4, phases and state log: `docs/PLAN.md` (Portuguese). The
repository format lives in the sibling XkyHold repository,
`docs/FORMAT.md` — the engine must match it; change the spec first, then
the code.

## Hard rules

- What is backed up is `$HOME` only: refuse source paths outside it, never
  follow symlinks while backing up, never run as root. A restore may go to
  any folder the user chooses (an external disk, for example); restoring
  to the original place stays inside `$HOME`.
- Encryption happens on the user's computer; destinations only see
  ciphertext. "Delete" in Drive is always "move to trash"; in a local
  folder it moves into the repository's `trash/`.
- Never regenerate `golden` fixtures (`crates/core/tests/fixtures/`). They
  are byte-for-byte copies of XkyHold's; a failing golden test means the
  change is wrong.
- Tests use temporary folders or the in-memory destination only. Backup
  never writes to the source. No restore to the original location in a
  real home folder without an explicit request.
- Automatic backup is opt-in (systemd user timer); the default is a
  reminder, like XkyHold web.

## Structure

Cargo workspace: `crates/core` (engine, no UI, no Tauri), `crates/dest-local`,
`crates/dest-drive`, `crates/cli`, `src-tauri` (thin Tauri commands over the
core), `ui` (Svelte 5 + Vite + TypeScript). The UI reaches the engine only
through Tauri commands and events; errors cross that boundary as a `kind`
string (`xkyhold_core::Error::kind`). Storage sits behind the `Destination`
trait.

## Development

- `npm install`, then `npm run dev` (Tauri window; Vite on port 5174 —
  5173 belongs to XkyHold web).
- `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`. Toolchain pinned in `rust-toolchain.toml` and
  `mise.toml`.

## Conventions

- Code, comments, commit messages, technical docs: English.
  `docs/PLAN.md` is European Portuguese.
- UI strings: English source, pt-PT translation addressing the user as
  "tu", never "você". Reuse XkyHold's strings where they fit.
- Commits: a title and bullet points. No local machine paths, personal
  email addresses or employer names in versioned files.
- Errors shown to users are actionable sentences; raw errors go to the log.
- Keep dependencies few and small.
