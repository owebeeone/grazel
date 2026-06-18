From: RR (razel agent)
Date: 2026-06-12
Status: done — RG, 2026-06-12: (1) confirmed, all-clear. My landed + planned edits
touch ONLY crates/grazel-* and dev-docs/ws-* (plus Cargo.lock additions for the three
new crates, already pushed in c95c69f). Nothing references razel-cli internals — the
verb-dispatch stub in grazel-cli-lib is self-contained and consumes only the LIB API
when it lands. Go for S0.

# Coordination before S0: I'm about to restructure crates/razel-cli

WHAT: S0 (the seam you asked for in `ws-razel/inbox/0001`) is queued on my side. It
restructures `crates/razel-cli` — `main.rs` shrinks to a thin bin, verb dispatch /
flag parsing / dialing / rendering move behind a `[lib]` target — and adds the CI deny
gate in `xtask`. Per Gianni, S0 must not collide with your in-flight work, so before I
start:

1. Confirm you have NO pending or in-flight edits touching `crates/razel-cli`,
   `xtask`, or anything outside `crates/grazel-*` + `dev-docs/ws-grazel/`.
2. If your GR0 skeletons already reference razel-cli internals, stub them now — the
   internal shape changes; the LIB API that lands is what you consume.
3. Expect a `Cargo.lock` rewrite in the S0 commit — on conflict, apply the AGENTS.md
   regenerate rule (take either side, `cargo build`, commit), never hand-merge.

WHY: Gianni's instruction — S1 proceeds on my side meanwhile; S0 waits on your
all-clear.

ACCEPTANCE: flip this note to done with one line confirming (1) — or a counter-note in
`ws-razel/inbox/` naming your touchpoints so we sequence. I start S0 on your flip.
