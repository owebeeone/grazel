# Host materialization (fetch R1): java import repo rule — records, never executes.
def _java_import_external_impl(ctx):
    pass

java_import_external = repository_rule(implementation = _java_import_external_impl)
