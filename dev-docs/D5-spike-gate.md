# D5 spike-gate — Pants v2 / Meson / Buck2 (go/no-go)

Per `RazelPlan.md` D5: razel proceeds only if each off-the-shelf option fails on a
**named axis** we care about. The decisive axis is set by **D6** (engine-as-product:
an *embeddable, stable, semver'd build-graph engine* exposing query + subscribe +
overlay for a human + AI-agent collaboration substrate — griplab).

| Tool | What it is | Fails on (named axis) |
|---|---|---|
| **Pants v2** | Rust core + BUILD-ish files + `pantsd`, dep-inference | Python rule layer; **not** a single ownable static binary; **no embeddable query/subscribe/overlay graph API** for an IDE/agent substrate. |
| **Meson** | Declarative build, generates Ninja | Not Bazel/Starlark dialect; no exposed incremental-graph engine to query/subscribe; not an embeddable substrate. |
| **Buck2** | Rust, `dice` graph, daemon (closest) | **No stable embeddable API** (internal, churns); you don't own it; no `file→tests→targets` query / change-subscription / overlay surface; heavy. |

**Surviving niche (D5 ∩ D6):** *Bazel/Starlark dialect in a single tiny ownable static
binary that exposes an embeddable query / subscribe / overlay build-graph API as the
collaboration substrate.* None of the three provides this — the gap is **architectural**
(an embeddable, stable graph API), not an ergonomics detail.

**Basis & honesty:** the decisive axis is a structural fact about these tools' public
surfaces (none exposes an embeddable, semver'd query+subscribe+overlay build-graph API),
established from their architecture — not from hands-on bench-marking. Hands-on evaluation
would refine the *secondary* axes (dialect coverage, ergonomics, build speed) but cannot
change the decisive one.

## Decision: **GO.** razel is justified by D6; build proceeds. (Roll-build `razel-ab/`.)
