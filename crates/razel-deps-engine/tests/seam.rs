//! Slice 1 acceptance (RazelDepsEngineV2.md §20): the legacy adapter accepts `Evaluate` and emits
//! the lifecycle, the loader's `SchedHook` round-trips through typed events, and the options digest
//! separates semantic options from scheduling-only ones.

use razel_deps_engine::{
    legacy_sched_point, CommandOutcome, DiagnosticCode, EngineCommand, EngineEvent, EngineOptions,
    EvalRoot, EvaluateRequest, LegacyDepsEngine, RazelDepsEngine,
};

/// A BUILD that yields one analyzed target AND fires a loader `depset` event (so the SchedHook
/// translation has something to carry).
const BUILD: &str = r#"
def _impl(ctx):
    d = depset(["a", "b"])
    ctx.actions.run(executable = "tool", outputs = [], inputs = [], arguments = [str(len(d.to_list()))])

r = rule(implementation = _impl, attrs = {})
r(name = "widget")
"#;

fn evaluate(engine: &LegacyDepsEngine, command_id: u64) -> razel_deps_engine::CommandToken {
    engine
        .submit(EngineCommand::Evaluate(EvaluateRequest {
            command_id: razel_deps_engine::CommandId(command_id),
            root: EvalRoot::BuildSource {
                name: "BUILD".to_string(),
                src: BUILD.to_string(),
            },
            options: EngineOptions::default(),
        }))
        .expect("submit")
}

/// REQ-DEPSV2-001, 024: `Evaluate(BuildSource)` → accepted, (diagnostics,) snapshot committed,
/// finished; and the committed result is readable by its `SnapshotId`, never off a live Session
/// (REQ-DEPSV2-005).
#[test]
fn evaluate_build_source_emits_lifecycle_and_commits_a_readable_snapshot() {
    let engine = LegacyDepsEngine::new();
    let sub = engine.subscribe();
    let token = evaluate(&engine, 1);
    let events = sub.drain();

    assert!(
        matches!(events.first(), Some(EngineEvent::CommandAccepted(_))),
        "first event is CommandAccepted, got {:?}",
        events.first()
    );

    // Exactly one snapshot is committed; CommandFinished::Ok names it; it is readable by id.
    let committed = events.iter().find_map(|e| match e {
        EngineEvent::SnapshotCommitted(s) => Some(s.snapshot),
        _ => None,
    });
    let committed = committed.expect("a snapshot was committed");

    let finished = events.last().expect("a terminal event");
    match finished {
        EngineEvent::CommandFinished(c) => match c.outcome {
            CommandOutcome::Ok { snapshot } => assert_eq!(snapshot, committed, "finished names the committed snapshot"),
            CommandOutcome::Err { ref message } => panic!("expected Ok, got Err: {message}"),
        },
        other => panic!("last event is CommandFinished, got {other:?}"),
    }

    let targets = engine.snapshot(committed).expect("snapshot readable by id");
    assert!(
        targets.iter().any(|t| t.name.contains("widget")),
        "committed snapshot carries the analyzed target: {:?}",
        targets.iter().map(|t| &t.name).collect::<Vec<_>>()
    );

    // Every event carries this command's id, and sequences are gap-free from 1.
    assert!(events.iter().all(|e| e.header().command_id == Some(token.command_id)));
    let seqs: Vec<u64> = events.iter().map(|e| e.header().sequence).collect();
    assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>(), "gap-free seq from 1: {seqs:?}");
}

/// REQ-DEPSV2-004, 015: the loader's stringly `SchedHook` becomes a typed event on the way out and
/// converts back to the old `(point, key)` via the legacy adapter — losslessly, here, for `depset`.
#[test]
fn sched_hook_round_trips_through_typed_events() {
    let engine = LegacyDepsEngine::new();
    let sub = engine.subscribe();
    evaluate(&engine, 1);
    let events = sub.drain();

    // The loader fired at least one `depset` point; it arrived as a typed Diagnostic.
    let depset_diag = events
        .iter()
        .find(|e| matches!(e, EngineEvent::Diagnostic(d) if d.code == DiagnosticCode::Sched("depset".to_string())))
        .expect("a typed depset diagnostic");

    // …and it round-trips back to the legacy point/key the old SchedHook tests assert on.
    let (point, key) = legacy_sched_point(depset_diag).expect("legacy point recovered");
    assert_eq!(point, "depset");
    assert!(key.starts_with("direct="), "the legacy depset key is carried verbatim: {key}");

    // Non-diagnostic events have no legacy point (the adapter is intentionally partial).
    let accepted = events.iter().find(|e| matches!(e, EngineEvent::CommandAccepted(_))).unwrap();
    assert!(legacy_sched_point(accepted).is_none());
}

/// REQ-DEPSV2-023: semantic options change the digest; scheduling-only / observability options do
/// not (so a thread-count change reuses semantic results).
#[test]
fn options_digest_tracks_semantic_options_not_scheduling() {
    let base = EngineOptions::default();

    // A SEMANTIC change (strict_bazel) moves the digest.
    let mut semantic = EngineOptions::default();
    semantic.semantic.strict_bazel = true;
    assert_ne!(
        base.options_digest(),
        semantic.options_digest(),
        "strict_bazel is semantic — it must change the digest"
    );

    // Another semantic change (a -D define) also moves it.
    let mut defined = EngineOptions::default();
    defined.semantic.defines.push(("FOO".to_string(), "1".to_string()));
    assert_ne!(base.options_digest(), defined.options_digest(), "defines are semantic");

    // SCHEDULING-only changes (thread count, event profile) must NOT move the digest.
    let mut scheduled = EngineOptions::default();
    scheduled.scheduling.threads = Some(6);
    scheduled.scheduling.event_profile = razel_deps_engine::EventProfile::DebugGraph;
    assert_eq!(
        base.options_digest(),
        scheduled.options_digest(),
        "threads + event profile are scheduling-only — they must NOT change the digest"
    );
}
