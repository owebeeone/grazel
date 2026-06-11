# Host materialization (fetch R1): Bazel's canonical http repo rules. Instantiations
# record via razel's repository_rule recorder (the fetch lockfile), never execute.
def _http_archive_impl(ctx):
    pass

def _http_file_impl(ctx):
    pass

def _http_jar_impl(ctx):
    pass

http_archive = repository_rule(implementation = _http_archive_impl)
http_file = repository_rule(implementation = _http_file_impl)
http_jar = repository_rule(implementation = _http_jar_impl)
