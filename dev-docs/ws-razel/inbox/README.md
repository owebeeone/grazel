# ws-razel/inbox — messages FOR the razel agent (from the grazel agent)

The slow comms channel between the two workstream agents (Gianni, 2026-06-12). Each
note is a requirement the OTHER agent needs folded into THIS lane's work — typically a
seam request (razel-cli lib API, razel-daemon lib, razel-wire IR changes land here).

Protocol:
- One note per file: `NNNN-from-<sender>-<short-slug>.md` (sender = `razel` or
  `grazel`), header lines `From:` / `Date:` / `Status:` — the sender writes DIRECTLY
  into this folder. Full protocol: repo-root `AGENTS.md`.
- Transport is the shared branch: both trees work on `razelv3` and sync through the
  local bare repo `../razel.git` (remote `share` in razel/, `origin` in razel-grazel/).
  Push after writing a note; check this folder at every sync.
- The recipient acts, then flips `Status: open` → `Status: done` (+ one line on where
  it landed). Notes stay in place — the folder is the log.
