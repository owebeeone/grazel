# Host materialization (fetch R1): git repo rules — record, never execute. The fetcher's
# git support (clone at commit) is an R2+ decision; specs carry remote/commit/tag.
def _git_repository_impl(ctx):
    pass

def _new_git_repository_impl(ctx):
    pass

git_repository = repository_rule(implementation = _git_repository_impl)
new_git_repository = repository_rule(implementation = _new_git_repository_impl)
