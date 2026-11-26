# Stacked PRs

GitHub's native stacked PRs are a set of pull requests whose **base branches form a chain**: the bottom PR targets your main branch, the next PR targets the bottom PR's head branch, and so on. GitHub then shows them as a stack. The important constraint is that the PR base refs must already be chained before GitHub will accept the stack.

`jj-gh` does not invent a separate stack format. It reads the shape of your local `jj` graph, finds bookmarks on those commits, and makes GitHub match that shape. It never rewrites your `jj` graph: shape the graph yourself with `jj rebase` / `jj new`, then publish or reconcile.

## How stacks are discovered

Stack detection is inference from two things:

1. **Bookmarked ancestors in the local graph.** For a revision, `jj-gh` asks jj for the tip-most bookmarked ancestors that are not already in `trunk()` (roughly `heads(ancestors((rev)-) & bookmarks() & ~::trunk())`) and takes one. That bookmark is the revision's "stacked ancestor".
2. **Open PRs whose head branch names match tracked bookmarks on the push remote.** A PR is only considered if its head ref name matches a local bookmark tracked on the remote used for pushes. Chains need at least two PRs; a single PR forms no stack.

The **local** bookmark target is used for detection, even if you rebased locally and have not pushed yet. That is deliberate: your local graph is the source of truth, and `jj pr stack` includes stale heads in its plan and pushes them as part of reconciliation.

```sh
# See the graph jj-gh reasons about, with PR metadata when PRs exist
jj pr log

# Inspect the ancestor/base detection for one revision
jj-gh debug rev <rev>
```

## Automatic linking on `pr create`

After creating PRs, `jj pr create` tries to link them into GitHub stacks. This is controlled by `[jj-gh] auto_stack = true` (the default) and can be overridden per invocation:

```sh
jj pr create uttltvvuqrvn          # auto-link by default
jj pr create --no-stack uttltvvuqrvn
```

Before linking, `jj-gh` gathers **all** local PRs, not just the ones created in this run. A new PR on top of an existing stack therefore folds into that stack instead of becoming a separate one beside it.

Two details matter:

- The bottom PR keeps whatever base you chose in the editor. Only bases _within_ a detected chain are realigned.
- Auto-linking is best effort. Individual PR-detail fetches that fail are warned about and skipped, and a stack-linking failure is warned about rather than failing the whole command. If linking did not happen, the PRs are still created; run `jj pr stack --dry-run` to inspect the remaining work.

## Creating multiple revs as a stack

`jj pr create` takes multiple revision arguments, each of which becomes its own PR. Pass them **bottom to top** so the second PR can use the first PR's newly pushed branch as its base:

```sh
# Publish two existing revisions, bottom then top
jj pr create uttltvvuqrvn lyvnmrkmuwov

# Equivalent, one at a time
jj pr create uttltvvuqrvn
jj pr create lyvnmrkmuwov
```

A revision without a bookmark gets a jj-generated bookmark when it is pushed
(by default, `push-` followed by its change ID). The second PR can then use the
first PR's generated bookmark as its base.

A revset is not expanded by `pr create`; use separate revision arguments for separate PRs. (By contrast, `jj pr stack` _does_ expand a range like `qv::uw` into a bottom-to-top chain.)

## Pushing stale stack members

When creating PRs that form a stack, `jj-gh` pushes the current rev as usual, then detects whether there are existing PRs below it in the chain. Any of those whose remote target has fallen behind the local one are refreshed in a single `jj git push` call. This avoids triggering unnecessary CODEOWNERS reviews from stale diffs.

```sh
jj pr create @- @             # pushes the rev, then any stale PRs below it
jj pr create --no-push @- @   # push only the current rev (no batch refresh)
```

The `auto_push` setting (default: `true`, same config key as `pr stack`) controls this behaviour. Pass `--no-push` to disable it, which falls back to pushing only the current rev's bookmark.

## Base selection order

For each PR, the base is resolved in this order:

| Priority | Source                                        | Notes                                                       |
| -------- | --------------------------------------------- | ----------------------------------------------------------- |
| 1        | `--base <branch>`                             | Explicit override.                                          |
| 2        | Closest bookmarked ancestor outside `trunk()` | The normal stacked-PR case. Uses the local ancestor commit. |
| 3        | `trunk()`                                     | jj's detected trunk bookmark.                               |
| 4        | Configured `default_base_branch`              | Only reached if `trunk()` does not resolve.                 |

The chosen base is shown in the editor frontmatter and may be edited there before creation; GitHub must have that branch. For an explicit or configured base, `jj-gh` resolves the revision from `<branch>@<remote>` rather than a same-named local bookmark, because the two can point at different commits.

## Limitations and escape hatches

- **Branching graphs can be ambiguous.** Detection picks one tip-most bookmarked ancestor via `heads(...)` and a limit of one. A merge, diamond, or several bookmarked parents can yield a surprising base or chain; there is no global topological sort.
- **Explicit targets are the escape hatch.** `jj pr stack 101 102` (or `jj pr stack 'qv::uw'`) asserts exactly that stack, bottom to top. Naming targets skips the confirmation prompt. See [Reconciling stacks](./stack-reconciliation.md).
- **Failures may be warned or suppressed during auto-linking.** `pr create` reports stack problems but does not undo PRs it already created.
- **Operations are not transactional.** Pushes, base updates, unstacks, and stack creations happen sequentially. A failure can leave GitHub partially updated; rerun `jj pr stack --dry-run` to see what remains.
- **Discovery only sees open PRs** whose head branch names match bookmarks tracked on the push remote. Renamed head branches, merged/closed PRs, and untracked bookmarks are invisible to automatic mode.
