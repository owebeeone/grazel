# Grazel Forecast — future-direction probabilities → trade-off choices

**Not a roadmap.** This reads `GrazelProposal.md` (Model G) and the mission directions as
**predictions with probabilities**, and uses `P × cost-to-retrofit` vs `cost-to-provision`
to choose trade-offs *now*. It operationalizes `ArchitectSkillRules.md` criterion (c):
calibrate indirection by the probability-weighted future. Probabilities are my
calibration from the confirmed grip-lab mission + `GrazelProposal.md` + the Bazel/Pants
trajectory — **edit them; you're the oracle.**

**Decision calculus:** **Commit** (high P, mission-core: build first-class) · **Hedge**
(uncertain P + expensive retrofit: spend a *small* relief valve — opaque type / interface
/ surface-agnostic seam — to keep the option open without paying for it) · **Defer** (low
P or cheap retrofit: hard-code now).

---

## Part A — Mission-derived directions

| # | Direction | P | Horizon | Trade-off it informs | Lean |
|---|---|---|---|---|---|
| D1 | F17 derivation server (IDE index, affected, lint, provenance) for MCP/UI | 0.95 | near–mid | facts/derivation layer; serializable providers | **Commit** |
| D2 | Distribution over iroh (per-platform nodes, fact-merge) | 0.9 | mid | F24; Session-as-value; serializable facts | **Commit** keystone |
| D3 | AI agents first-class (MCP queries/edits) | 0.9 | near–mid | query/subscription surface; provenance (F21) | **Commit** query+provenance |
| D4 | Warm daemon/server as default (not CLI one-shot) | 0.85 | near | Engine as the primary live path | **Commit** |
| D5 | Cross-platform via multi-instance (N single-config graphs) | 0.8 | mid | provider key shape; config model | **Commit** → key by `Label` |
| D6 | Toolchain diversity / cross-compilation | 0.75 | mid | toolchain resolution | **Commit** toolchain-as-matcher; kill `CXX`/`AR` consts |
| D7 | `@repo` → local-checkout mapping | 0.7 | mid | external-dep seam | **Commit** path-map behind a resolver interface |
| D8 | More languages (go/java/ts/proto) | 0.6 | mid–far | open-set composition | **Defer** — manifest registry already absorbs it |
| D9 | User-authored custom `rule()`s | 0.6 | mid | freezable rule() + schema-driven attrs | **Commit** the typed-attr work (planned) |
| D10 | Parallel *execution* (F7) | 0.6 | mid–far | the `!Sync` Engine rewrite | **Hedge** — keep Engine `Send`-friendly; don't build scheduler yet |
| D11 | Remote execution | 0.5 | far | execution-agnostic logic (F22) | **Hedge** — action/spawn behind interface |
| D12 | In-graph `(Target×Config)` transitions / exec-host | 0.35 | far | `Label`→`(Label,Config)` key migration | **Hedge** — opaque `TargetKey` newtype; bounded exec/host only if forced |
| D13 | Parallel *analysis* | 0.4 | far | lifting analysis into the graph | **Defer** — D5 covers the dominant case |
| D14 | Full external-dep *fetch* + lockfile | 0.4 | far | the repo resolver | **Defer** — slots behind D7's interface |

---

## Part B — Grazel / Model-G, decomposed as predictions

The key result of reading `GrazelProposal.md` as a forecast: **Model G is not one
low-probability speculative future. Its *core* is high-probability because it is largely
*entailed by the already-committed F17/F24 mission*; only its authoring *surface* is the
speculative, deferrable part.** Decompose it:

| # | Model-G component | P | Why | Trade-off / lean |
|---|---|---|---|---|
| **Gc1** | **Canonical typed (serializable) contract as the center; all surfaces lower into it** | **0.9** | This *is* the F17/F24 fact substrate by another name — a derivation server distributing facts over iroh **requires** a canonical typed contract. (GrazelProposal §4, §4.3.) | **Commit.** Make the IR the canonical *product*, typed + serializable (taut/CBOR) — never a Bazel-internal struct. *No-regret; double-justified (mission + Model G).* |
| **Gc2** | **MCP surface: schema / query / explain / provenance / transactional edit** | 0.85 (query+explain+provenance); 0.6 (propose/validate/apply) | D3 (agents first-class) concretized. (GrazelProposal §9.) | **Commit** query+explain+**provenance (F21)** as design drivers; **defer** the transactional-edit ops behind the query surface. |
| **Gc3** | **Deterministic, provenance-carrying inference as an analysis pass** (source roles, include-deps) | 0.6 | Strong agent-assist value; reduces authoring. Must stay a deterministic pass over *declared scan inputs* (preserves F1/F2/F4/F10). (GrazelProposal §4.2.) | **Hedge** — the fact model already accepts derived-facts-with-provenance; don't build inference passes yet, don't preclude them. |
| **Gc4** | **The `.razel` sparse descriptor surface** (`SELF.razel` + sidecars + merge-classes/precedence) | 0.45 | The most speculative *product-UX* bet; the spelling is explicitly open (GrazelProposal §15.1). | **Defer — but free-hedged:** if Gc1's contract is surface-agnostic (high-P anyway), this slots in later as one more adapter at **zero core cost**. |
| **Gc5** | **Bazel BUILD/`.bzl` as one adapter lowering to the contract** | 0.9 | Already the front-end→IR seam. (GrazelProposal §10.) | **Commit** — it's the seam discipline we already recommend. |

**The synthesis line:** treating Grazel as a prediction adds *no new architectural
commitments* — it **raises confidence** in the trade-offs the F17/F24 mission already
favors (typed-serializable canonical contract + MCP query/provenance + adapter front-end),
because they now serve **two** predicted futures at once. The only genuinely-new bet is the
`.razel` surface (Gc4, P≈0.45), and the clean contract keeps it a **free option**.

---

## Part C — Trade-off ledger (open questions, resolved by the probabilities)

- **Q1 provider key** → D5(0.8) > D12(0.35): **key by `Label`**, wrapped in an opaque
  `TargetKey` newtype (the D12 hedge). Gc1 adds: the key/fact types must be **serializable**.
- **Q2 multi-config** → D5: cross-platform = multi-instance, **not** in-graph cross-product;
  bounded exec/host only if D12 fires. Don't build config-as-axis.
- **Q3 F17 committed?** → **Yes** (D1=0.95; reinforced by Gc1/Gc2).
- **Q4 parallel analysis vs exec** → D10 hedge > D13 defer: **execution-parallelism only**,
  workload-paced; Engine kept `Send`-friendly.
- **Q5 analysis early-cutoff digest** → contingent on D13 (deferred) → **defer**.
- **Q6 `ctx.actions.run` → IR lowering for evaluated `.bzl`** → **verify regardless**; Gc5
  elevates it (the adapter must lower faithfully).
- **New, promoted by Gc1/Gc2** → the **taut canonical-contract schema** (typed serializable
  providers/facts) and the **MCP query/explain/provenance surface** are first-class design
  drivers, *not* afterthoughts. The **`.razel` authoring surface (Gc4) is deferred** behind
  the free surface-agnostic-contract hedge; descriptor *syntax* (GrazelProposal §15.1)
  stays an open decision we **don't** make now.

**Net architectural lean (forecast-weighted):** A1 spine + typed **serializable** providers
forming a **canonical contract (the IR is the product)** + the F17 derivation layer over a
distributable fact substrate + MCP **query/explain/provenance** surface + toolchain/select
matchers. `TargetKey` and `Send`-friendly Engine are the two cheap hedges (D12/D10). The
Bazel front-end is *an adapter*. The `.razel` descriptor surface and inference passes are
deferred options the clean contract preserves for free.

---

## Revisit triggers
- A grip-lab spec names a consumer/derivation → raise D1/D3/Gc2; refine the contract schema.
- Authoring friction on BUILD files becomes a real complaint → Gc3/Gc4 jump; revisit the
  `.razel` surface decision (the contract already supports it).
- A second config *in one graph* is requested → D12 jumps; re-evaluate Q1/Q2.
- An MCP agent needs to *edit* (not just query) → Gc2's transactional ops jump.

*A forecast, not a plan. Choose trade-offs by the probabilities; update them as the future
stops being a prediction. `GrazelProposal.md` remains the (unmodified) vision being weighted.*
