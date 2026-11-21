# Reconciling stacks

`jj pr stack` makes GitHub match the shape of the local `jj` graph. The local graph is the source of truth; this command **never rewrites it**. Shape the graph yourself (for example with `jj rebase`), then reconcile.

Without arguments it discovers every local PR, shows the `jj log` it is working from plus a proposed plan, and asks for confirmation before writing anything to GitHub. With explicit PR numbers or revsets it asserts exactly that stack instead, which is the escape hatch when automatic detection is ambiguous.

## Preview and confirm

```sh
# Inspect first: prints the annotated jj log + plan, changes nothing
jj pr stack --dry-run

# Machine-readable plan on stdout (implies --dry-run)
jj pr stack --json | jq '.chains, .bases, .pushes'

# Interactive: prints the plan, then "Apply this plan? [y/N]"
jj pr stack
```

Automatic mode requires both `STDIN` and `STDERR` to be a terminal to prompt. Outside a terminal it prints the plan and stops; add `--force` to apply:

```sh
jj pr stack --force
```

`--force` also unstacks PRs that are already in a different stack. Answering `y` at the interactive prompt has the same meaning. UI (log, plan, prompt) goes to `STDERR`; only `--json` writes a result to `STDOUT`.

The plan has four sections:

| Section         | Meaning                                                                |
| --------------- | ---------------------------------------------------------------------- |
| `Push`          | Bookmarks whose remote target is behind the local target.              |
| `Retarget base` | PRs whose GitHub base ref will change to the proposed ancestor.        |
| `Stack`         | Chains to create or recreate, shown top to bottom.                     |
| `Unstack`       | PRs leaving their stack for good because no local chain contains them. |

## Asserting an exact stack

Pass PR numbers or revsets **bottom to top**. Naming the targets is itself the confirmation, so there is no prompt; `--dry-run` and `--json` still stop before any write.

```sh
jj pr stack 101 102 103

# A range expands bottom-to-top, skipping interior commits of multi-commit PRs
jj pr stack 'qv::uw'

# Mixed numbers and revsets are fine, as long as the order is bottom-to-top
jj pr stack 101 cli
```

Explicit mode is strict: a bookmarked commit with no open PR is an error rather than a skip, and a stack needs at least two PRs. If those PRs already sit in two different GitHub stacks, pass `--force` to unstack and recreate them.

## Pushing stale bookmarks

By default, `auto_push = true` (configurable) and PR heads whose remote target has fallen behind the local one are pushed before stacks are created/recreated. GitHub rejects a stack whose head branches have not been pushed, and a retargeted PR would otherwise keep showing commits from the old base.

```sh
jj pr stack --no-push          # plan/apply without pushing stale heads
jj pr stack --push             # explicit opt-in (overrides config)
```

## Apply order and why it matters

The plan is applied in this order:

1. **Unstack.** GitHub refuses to change the base ref of a PR that is in a stack.
2. **Update bases.** GitHub's stack creation only accepts PRs whose base refs already chain into each other; without this you get `422: Pull requests must form a stack`.
3. **Push stale bookmarks.** Done after base changes so the transient three-dot diff stays bounded (a stale head on a newer trunk drags extra commits into the diff and can trigger noisy CODEOWNERS review requests).
4. **Create/recreate stacks.** Any stack torn down in step 1 is rebuilt here; PRs no longer in a local chain stay unstacked.

Each base change only moves GitHub's `baseRefName`; nothing is rebased in jj or Git.
