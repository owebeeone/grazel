# GrazelCrates — the grazel crate structure (GR0 Deliverable 1)

*2026-06-12, RG. Answers `inbox/0001` — the design decided BEFORE crates are created.
Constraints inherited, not relitigated: `RazelPublicSurfaces.md` §1b/§1d/§1e,
`GrazelWorkstream.md` §0 (ownership, taut-everywhere, thin-bin rule, e2e TDD).*

## The crate set (three, no more yet)

| crate | kind | contents |
|---|---|---|
| `grazel-cli` | bin (`grazel`) | rust/OS mechanics ONLY: argv/env intake, exit code, process entry. One call into the lib. |
| `grazel-cli-lib` | lib | ALL business logic: verb dispatch, scope resolution, paths, daemon run/dial, the `ws test` stage registry. |
| `grazel-node` | lib (placeholder) | Compiles, one test, no design. Iroh arc gets its own phase doc (§0). |

## The inbox questions, answered

**1. Where does the scope/daemon machinery live?** In `grazel-cli-lib`, as modules
(`scope`, `paths`, `daemon`) — NOT a separate `grazel-daemon` crate. Reasons: (a) the
daemon side is currently thin glue over the `razel-daemon` LIB (allowed direction;
`Server::serve` already does transport + dispatch) — a crate around glue is boilerplate;
(b) the thin-bin rule needs the logic in-process testable, which a lib module satisfies;
(c) grazeld is `grazel` in daemon mode (§1d one-binary rule) — there is no second bin to
justify a second crate. **Split trigger** (recorded, not scheduled): when GR3 service
wiring or GR4's HTTP edge gives the daemon side real weight, carve `grazel-daemon` out
then — the module boundary is the future crate boundary.

**2. Where does the `ws test` stage registry live, and the stage type?** In
`grazel-cli-lib::wstest`. A stage is declarative data + a check fn:

```rust
pub struct Stage {
    pub name: &'static str,          // stable id; --stage=<name> selects it
    pub run: fn(&StageCtx) -> Result<(), String>,
}
pub struct StageCtx {
    pub grazel_bin: PathBuf,  // the REAL binary under test (current_exe as the verb;
                              // CARGO_BIN_EXE_grazel under cargo test)
    pub tmp: PathBuf,         // per-stage scratch root (isolated GRAZEL_HOME etc.)
}
```

`stages()` returns the ordered ladder; the verb and `cargo test` run the SAME list
(§1: the same stages serve both). Fixture needs stay inside the stage fn until two
stages share a fixture — then a `fixtures` module, not earlier.

**3. HTTP/WS edge crate now?** No. No consumer until GR4; pre-creating it is the
boilerplate smell the inbox itself flags. GR4 decides crate-vs-module with the split
trigger above on the table.

**4. Dependency diagram** (arrows = `depends on`; razel-* never points at grazel-*):

```
grazel-cli ──► grazel-cli-lib ──► razel-daemon (lib: Server, transport, client call)
                    │        ├──► razel-wire   (taut types: VersionInfo, …)
                    │        └──► razel-cli lib  [POST-S0 — touchpoint STUBBED today]
                    └──► grazel-node (placeholder; iroh-era)
```

The razel-cli-lib touchpoint (verb dispatch / bazel_flags / rendering) is stubbed as a
minimal arg walk inside `grazel-cli-lib` until S0 lands (`ws-razel/inbox/0001`); the
stub dies the day S0 arrives — it gains no features in the meantime.

## Scope/UDS layout implemented this round (§1e made concrete)

- Socket: `<grazel home>/.uds/<scope>` — flat, short (macOS 104-char UDS cap checked,
  fail loud). State: `<grazel home>/scopes/<scope>/daemon.json` (pid, build version,
  wire protocol). Grazel home = `$GRAZEL_HOME` if set (tests/fixtures), else `~/.grazel`.
- Scope names: `[a-z0-9][a-z0-9_-]*`, ≤32 chars — keeps sockets short and paths flat.
- Override chain (highest wins): `--scope` → `GRAZEL_SCOPE` → `.grazelrc`
  `service_scope=` → `default`. This round parses ONLY `service_scope` out of
  `.grazelrc`; the full rc layer is GR2.
- Razel's daemon namespace is `_razel_<user>/daemon/` (§1e) — disjoint from
  `<home>/.uds/<scope>` by construction; no shared prefix, no flag can cross them.
- Hello v0 = the existing `version` method (`VersionInfo`: build version + protocol).
  The §1e hello also carries the WORKSPACE ROOT — that needs a new taut message,
  which is razel-side IR work: seam-requested via `ws-razel/inbox/` (GR1 blocker noted
  there, not here).

## Debts opened by this design

- razel-cli stub (dies at S0 pull).
- Hello-with-workspace-root awaits razel-wire IR growth (seam request filed).
- `grazel-cli-lib` owns daemon glue until the GR3/GR4 split trigger fires.
