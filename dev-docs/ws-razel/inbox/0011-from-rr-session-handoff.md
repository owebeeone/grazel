From: RR (previous session, 2026-06-13)
Date: 2026-06-13
Status: open — flip to done once you've oriented and own the queue.

# RR → RR: session handoff — orient here, then take the queue

You are RR, the razel-lane agent (repo-root `AGENTS.md` is the contract: identity by
tree, the inbox comms protocol with RG, the sync ritual, the razel→grazel crate/labor
arrow). Tree: `razel/` on branch `razelv3`, share = local bare `../razel.git` (remote
`share`). RG (grazel agent) works the sibling `razel-grazel/` clone. Gianni is the
human; he often tests rigor — verify before asserting, concede cleanly.

## Orientation ritual (do these, in order)

1. `git pull --rebase share razelv3` — then read BOTH inboxes for `Status: open`
   (`dev-docs/ws-razel/inbox/` = yours; `ws-grazel/inbox/` = what you owe RG flips on).
2. `dev-docs/ws-razel/RazelV3Checkpoint4Precis.md` — read the LAST few round deltas
   (rounds 47–53+ are this campaign); it is the running truth of what banked and why.
3. `parity/examples/SURVEY.md` — the examples burn-down table (regenerate with
   `cargo xtask examples --survey`); `parity/examples/README.md` for goldens status.
4. Health: `cargo test --workspace` (expect ~79 suites green), `cargo xtask gates`,
   `cargo xtask probe` (sentinels INCLUDE the examples goldens). TF floor:
   `cargo xtask tfload` — the metric FLUTTERS 453–455 (band characterized round 53;
   ±1 A/B attribution is below its noise floor; banding/pinning it is a named debt).

## State at handoff (2026-06-13)

**Spike (ws-razel/RazelReleaseSpike.md, V3sh1):** S0–S4 COMPLETE (tags razelV3/s0-seam,
s1-emode, s2-npm, s3-complete, s4-complete). The gryth-bootstrap bar is closed BOTH
sides. S5 is mid-flight folded into the examples burn-down (Gianni: every verb per
example, TEST especially): `razel test` landed (exit 0/3/1 protocol, testlogs,
summary lines), `run` landed, rc-lite landed, daemon path rides the loader-capable
pipeline (RG 0008). Examples: cpp-tutorial goldens green (graph+stdout, harness is a
probe sentinel; ONE documented argv deviation: bzlmod -iquote set). Survey: 18/44
workspaces fully green.

**Bazel-compat queue (mine), roughly in order:**
- @npm shim design note (aspect npm_link_all_packages over fetch-npm's materialization)
  — RG holds a REVIEW SEAT, flag them before implementing. Unlocks frontend (16 pkgs).
- The unvendored-repo conveyor: @crates, @com_google_absl, @rules_go, @rules_kotlin,
  @rules_android, @rules_oci, @aspect_bazel_lib… — same fetch/stub machinery as the
  TF-era conveyor (xtask fetch / host.rs rows). Biggest single lever in the survey.
- ctx.executable/<attr> resolution in rule() impls (configurations/cc_test's row;
  several custom-rule examples will share it).
- User-defined make variables + toolchains attr (make-variables row).
- Floor banding/pinning debt (URGENT-tagged round 53): band the TF floor or pin the
  order-dependent classes.
- Later ladder: S6 (full bazelrc/--strict_bazel/discovery; query verb SPEAKS BAZEL'S
  query-expression surface — rdeps() etc.; `affected` is sugar over it, golden vs
  `bazel query rdeps`), S7+ per spike. Oracle pins are PER TIER (§3): examples =
  machine bazel 9.1.1; TF = corpus 7.7.0.

**Grazel coordination state:** RG's ladder ~29 stages green. They hold: grazel test
wiring (verbatim delegation v1 — decided), gryth-examples fixture corpus (authoring
next), @npm review seat (reactive), doctor + gap-fillers. Their View seam
(GrazelViewSeam.md, KeyedFanout) is PRODUCER-SWAP-READY: **GR5b is gated on MY View
service** (the §1c committed-snapshot + §4b delta work — the big razel-side item on
the PublicSurfaces roadmap; query-snapshot discipline rides the same work). D15 (taut
TS runtime) is GIANNI'S, not a seam of ours. The shutdown loop is productized
(grazel shutdown verb + outlock hint names it).

**House rules that bind you (learned the hard way, don't relearn):**
- NO-COMPETING-SURFACES (spike §3c): existing bazel-ecosystem surface → faithful
  subset, never a razel dialect (@razel_js died for @aspect_rules_js same-day).
- TDD: failing test first, every bank green (suite+gates+probe; tfload when the
  engine's TF path is touched — cite the band, don't chase ±1).
- Ground-truth experiments before compat claims (tools: machine bazel; controlled
  fixtures); probe BEFORE commit (round-40 lesson).
- Inbox: delivery is the push; flip Status with one line on where it landed; batch
  seam requests; coordinate BEFORE touching anything RG consumes (flags coordination
  = ws-grazel/inbox/0008, RG picked option…check its flip).
- Commits: terse, ≤3 lines, no Co-Authored-By. Tags razelV3/<step>; re-point after
  rebase (tags orphan).

ACCEPTANCE (yours): orientation done, this note flipped, and the queue above either
taken in order or re-ordered with a reason in your first round delta.
