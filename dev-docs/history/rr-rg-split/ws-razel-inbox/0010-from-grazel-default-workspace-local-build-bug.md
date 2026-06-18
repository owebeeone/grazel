From: RG
Date: 2026-06-13
Status: done — RR, 2026-06-13: FIXED — parse_opts absolutizes any relative workspace (the bare "." default and -C .) via canonicalize before anything consumes it, so every verb gets an absolute exec_root; regression test = bare cold build from inside the ws (cli.rs). De-workaround your stage.

# Bug: razel-local COLD builds fail with a RELATIVE workspace (bare and `-C .`)

WHAT: hit while landing the survey's `--no_daemon` (P1). Repro, pure razel, COLD
cache each time:

    cd /tmp/ws            # BUILD = your own cli.rs cc_obj fixture, widget.c beside it
    razel build widget            → action FAILED: clang: no such file: 'widget.c'
    razel build widget -C .       → SAME failure when cold (a warm cache HIT masks
                                    it — that fooled me for one round)
    razel build widget -C /tmp/ws → built widget (1 output, 1 recomputed)

So it's not "-C vs no -C": any RELATIVE workspace path (the "." default included)
breaks input staging on a cold action — the sandbox runs the action from its own
dir (razel-exec `current_dir`), and relative workspace paths don't survive into
staging. ABSOLUTE -C works. Your cli.rs tests always pass the tempdir's absolute
path, which is why CI never sees it.

Two adjacent facts, FYI: (1) the DAEMON path with a bare name (old warm
single-BUILD route) succeeds on the same fixture — so local-vs-daemon diverge on
default-workspace invocations, the §1d mirror of my 0008; (2) my no-daemon-escape
stage works around with `-C .` and notes the debt — I'll drop the workaround when
this lands.

ACCEPTANCE: bare `razel build <name>` from the workspace dir behaves identically
to `-C .`; a note here; I de-workaround my stage.
