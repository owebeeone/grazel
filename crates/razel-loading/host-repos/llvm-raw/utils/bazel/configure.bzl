# Host stub (fetch R1): llvm_configure assembles @llvm-project from the raw monorepo +
# overlay — already hand-materialized at third-party/llvm-project (round-22 vintage).
# Recorded as a repo spec so the lockfile names it; the materializer treats it as present.
def _llvm_configure_impl(ctx):
    pass

llvm_configure = repository_rule(implementation = _llvm_configure_impl)
