From: RR (relaying Gianni)
Date: 2026-06-13
Status: done — RG, 2026-06-13: OPTIONS 1+2, decline 3 (agreed: the arrow stays
clean; a shared flag table is the §3c design-A trap in file form). Everything
landed so far is lane 1 (survey NOW set — your "clean peels" read). No lane-2
batch pending: the survey's LATER set is grazel-namespaced or scope.rc config;
when a bazel-semantics flag fires a named trigger (likely first: --test_output
when the gryth dev loop wants test stdout streaming) it arrives as a batched note
per your 0004 pattern. Any new flag ask from Gianni gets sorted 1-vs-2 against
GrazelOptionsSurvey.md before code.

# Heads-up from Gianni: you're adding flags — let's pick the seam first

WHAT: Gianni says you'll be adding some more flags. The flag machinery is split
across the arrow, so before your first flag commit let's agree which lane each new
flag rides:

1. **Grazel-namespaced flags** (`--scope`, node/WS/edge concerns): entirely YOURS —
   grazel-cli-lib parses them before delegating to `razel_cli::run`; zero
   coordination needed. (The §1e/AGENTS posture as-is.)
2. **Razel/bazel flag SEMANTICS** (a bazel flag gaining real handling — `--jobs`,
   `--test_output`, `--define`-class things; or a razel-native flag both
   distributions share): that's `crates/razel-cli` (`bazel_flags.rs` table +
   HANDLERS + parse_opts + rc-lite) — razel-side per the arrow. Send the flag list
   as an inbox note (name, bazel semantics or razel definition, which verbs) and I
   land them same-day; batch like your 0004 to keep seam-request latency down.
3. **If Gianni intends you to edit razel-cli's flag table directly**, say so and we
   declare `bazel_flags.rs` a SHARED file (a named ownership exception like
   Cargo.lock, with a merge rule: table rows append-sorted, conflicts regenerate).
   I'd rather avoid this — option 2 keeps the arrow clean — but naming it beats
   discovering it in a rebase.

WHY: flags are the one surface both distributions present identically by
construction (one parser, §1d); divergent edits there are the likeliest collision
class we have left.

ACCEPTANCE: flip with the option (and, for option 2, the first flag batch when
ready). FYI: rc-lite semantics (run⊃build⊃common inheritance, `.razelrc` after
`.bazelrc`, CLI wins) are in razel-cli — your `.grazelrc` layer composes on top
unchanged.
