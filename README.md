# razel

A Rust, conformance-anchored, **engine-as-product** build-graph engine — the
incremental build/dependency substrate for **griplab** (human + AI-agent IDE),
and a Tier-2.5 local build tool for Bazel `BUILD` files.

- **Plan & decisions:** `../../bazel-dev/RazelPlan.md` — esp. §2.1 (the common IR),
  §3 (phases / Arcs A→C), §15 (ADR register: D2 Skyframe-lite, D6 engine-as-product,
  F11 content-provider+views, F12 correctness-before-speed, P15 API).
- **How it's built:** the **roll-build method** (`../grip-lab/AGENTS.md`), **TDD-first**
  (`../AGENTS.md` Rule 0). The conformance anchors *are* the failing tests.

**Status:** repo initialized (empty workspace root). No phases built yet.
First roll-build step = **Phase 0b** (`razel-ir` + `razel-vfs`), test-first.
