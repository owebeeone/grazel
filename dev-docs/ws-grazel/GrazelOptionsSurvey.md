# GrazelOptionsSurvey — the grazel option surface: what exists, what to add NOW

*2026-06-13, RG (Gianni's ask: survey all command options addable now; plan each;
expectation that only a small set makes sense yet). Scope: the GRAZEL-namespaced
surface only — razel/bazel flags are razel-lane (the generated bazel_flags table,
recognized-but-diagnosed) and reach grazel verbatim by delegation (§1d).*

## 1. Inventory — what exists today

**Verbs:** `version` · `scope` · `daemon run|ping|stop` · `ws test` ·
`build`/`affected` (daemon-routed razel verbs) · `run` (streamed) · every other
razel verb by verbatim delegation.

**Grazel-namespaced flags:** `--scope=` (peel-anywhere) · `--workspace=` ·
`--no_autostart` · `--stage=` (ws test) · `--idle-timeout=` / `--member-idle-timeout=`
/ `--http-bind=` (daemon run). **Env:** `GRAZEL_SCOPE`, `GRAZEL_HOME`. **rc:**
`.grazelrc service_scope=`; `scope.rc pinned=`.

**Razel-only flags that pass through and matter here:** `-C/--workspace`,
`--daemon`, `--socket`, `--cbor` (explicit routing wins verbatim, GrazelVerbs.md).

## 2. The survey (verdict per candidate)

| candidate | verdict | why |
|---|---|---|
| `grazel shutdown [--scope] [--all]` | **NOW (P0)** | Gianni-requested (inbox 0007): no-idle-out default makes lingering daemons normal; kill-by-pid is not a product answer |
| `--no_daemon` on build/affected/run | **NOW (P1)** | The escape hatch: razel-local semantics under grazel when the daemon path lags (yesterday's loader gap was exactly this). Mirrors razel's `--daemon`, inverted |
| `grazel daemon status [--scope]` / `grazel scope --list` | **NOW (P2)** | Stale-daemon day showed the need to SEE state: pid/port/members per scope, liveness-checked. Filesystem-only (daemon.json + members/) — no wire work |
| `grazel ws test --list` | **NOW (P3)** | Trivial; the ladder is 23 stages and `--stage=` is blind without it |
| `http_port=` in scope.rc | LATER | gryth dev wants a stable port eventually; config-as-data (scope.rc), not a flag. Trigger: first gryth-ui attach that can't read daemon.json |
| `--json` on scope/ping/status | LATER | Machine-readable CLI output; gryth tooling trigger. (`--cbor` stays razel's spelling) |
| `member_idle_timeout=` / `idle_timeout=` in scope.rc | LATER | Flags exist for stages; per-scope CONFIG belongs in scope.rc when a real scope wants it. Don't duplicate until then |
| dial-patience knobs (`--connect-timeout`…) | LATER | Hardcoded 10s patience is fine until someone hits it; bazel precedent exists, add on first complaint |
| `--http-port=` flag | NO | Port is daemon-side state; pinning belongs in scope config (see http_port=), not per-invocation flags |
| `--output_base` overrides | NOT OURS | Engine knob — `.razelrc` territory (spike §3), razel lane |
| identity/mesh/auth flags | NOT YET | Iroh-era; §0 forbids designing it now |
| `--batch` (bazel's spelling) | NO (alias question) | Same meaning as `--no_daemon`; bazel's `--batch` is a STARTUP option with baggage. One spelling, ours — revisit only if bazel-muscle-memory complaints arrive |

## 3. Plans for the NOW set

**P0 `grazel shutdown [--scope=<s>] [--all]`** — top-level verb (razel has no
`shutdown`; no shadow). Default scope via the §1e chain; `--all` sweeps
`~/.grazel/.uds/*`, graceful-stops each live daemon, reports one line per scope;
idempotent (a stopped scope is calm, exit 0); `--scope` + `--all` together is an
error. Server side exists since GR1 (shutdown method → RAII lock release →
socket/daemon.json cleanup). Acceptance: `shutdown-verb` ws-test stage (two live
scopes; targeted stop kills one, `--all` kills the rest, artifacts cleaned).
RR amends the outlock "held by" hint to advertise the verb (their 0007 offer).

**P1 `--no_daemon`** — peeled (grazel-namespaced) on the routed verbs; skips
ensure-daemon + injection, delegates to razel_cli verbatim → razel-local
semantics. No daemon is started, no lock taken by a daemon (the local build takes
it itself, as razel does). Acceptance: stage asserting `grazel build --no_daemon`
succeeds with NO daemon.json/socket created for the scope.

**P2 `grazel daemon status [--scope]` (+ `scope --list`)** — read-only:
daemon.json (pid → liveness-checked, version, protocol, http_port) + members/
(kind + root per member) + socket presence, for one scope or `--list` for all
under the home. key=value lines (ws-test-parseable, same convention as `scope`).
No wire calls — observability must work on a WEDGED daemon. Acceptance: stage
covering live, dead-stale, and empty-scope renderings.

**P3 `ws test --list`** — print stage names, exit 0. Acceptance: trivial assert.

Sizing: P0+P3 land together in one small round (P0 is this round — Gianni asked);
P1 a half-round; P2 a round including its stage. Everything LATER has a named
trigger above; nothing else should grow the surface until a trigger fires
(boilerplate discipline — flags are surface area to keep green forever).
