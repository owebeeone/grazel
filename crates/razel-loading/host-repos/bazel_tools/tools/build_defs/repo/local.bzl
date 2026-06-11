# Host materialization (fetch R1): local repo rules — record, never execute.
def _local_repository_impl(ctx):
    pass

def _new_local_repository_impl(ctx):
    pass

local_repository = repository_rule(implementation = _local_repository_impl)
new_local_repository = repository_rule(implementation = _new_local_repository_impl)
