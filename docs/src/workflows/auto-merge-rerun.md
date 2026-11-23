# Auto-merge and CI reruns

## Auto-merge

`jj pr auto-merge <targets>` (alias `jj pr am`) enables or changes auto-merge on one or more PRs. Targets may be PR numbers and/or jj revsets; a revset can match a whole stack (for example `qv::uw`) and is resolved through the commits' local bookmarks, bottom to top.

```sh
jj pr auto-merge 1234
jj pr auto-merge zqxy                 # revision with a bookmarked PR
jj pr auto-merge qv::uw --method squash
jj pr auto-merge 1234 --stack         # cover the whole GitHub stack
jj pr auto-merge 1234 --cancel        # cancel the covering request
```

By default each target PR gets its own auto-merge request (GitHub's `includeStack: false`).

- `--stack` moves the covering request to the top of the target PR's **actual GitHub stack** — including PRs above the one you named and members with no local bookmark — and enables whole-stack auto-merge there. If no covering request exists, one is created at the stack tip. A PR that is not in a stack is reported rather than silently given a stack request.
- `--cancel` cancels the request covering each target PR, preferring a whole-stack request over an individual one. PRs with no covering request are a no-op. Use this when a request is stuck or before reshuffling a stack.
- `--stack` and `--cancel` are mutually exclusive.
- `-m` / `--method` chooses `merge` (default), `squash`, or `rebase`, overriding `auto_merge_method` from [Configuration](../configuration.md). When `--stack` moves an existing request, its method is preserved unless you pass `--method`.

Use `jj pr auto-merge` to change an auto-merge request or its merge method; `jj pr edit` is for other PR metadata.

- **Multiple targets:** a failure on one PR does not stop the others. Each failing PR is logged, and the command exits with a summary error such as `failed to enable auto-merge on 1 of 3 PR(s)`. If only one PR was targeted, its real error is reported directly.

### Auto-merge scope

The scope keywords are available in `[jj-gh] auto_merge`, the `auto_merge:` line in the editor frontmatter, and `pr create --auto-merge`:

| Value   | Meaning                                                                                                      |
| ------- | ------------------------------------------------------------------------------------------------------------ |
| `false` | Off (the default). Cancels any covering request when selected in the editor.                                 |
| `infer` | Whole stack for a stacked PR, individual PR otherwise. A bare `--auto-merge` means this.                     |
| `all`   | Whole GitHub stack through its top PR, falling back to this PR when it is not stacked.                       |
| `below` | This PR and the eligible PRs beneath it in its GitHub stack; an individual request, not promoted to the tip. |

At creation, `pr create` captures the frontmatter intent and applies it **after** stack linking: one request is made per linked stack, selected by the highest requesting PR and, for whole-stack scopes, moved to the stack's actual top PR. A PR set to `false` does not initiate a request and is never used as the endpoint, but it does not veto other PRs. Conflicting merge methods in a single `pr create` run abort before any auto-merge call; use `jj pr auto-merge` to settle on one method. If linking fails or you decline its confirmation, auto-merge is skipped and the PRs are still created.

Explicit CLI values require `=`: `--auto-merge=all`, `--auto-merge=below`, `--auto-merge=infer`, `--auto-merge=false`. `--no-auto-merge` is shorthand for `false`. Boolean `auto_merge = true` in config is deprecated and treated as `infer`.

### Merge queues

Repositories whose base branch uses a GitHub merge queue may reject `enablePullRequestAutoMerge`. `jj-gh` still sends the request and reports GitHub's error verbatim, adding a hint when it knows the base has a merge queue; it does not refuse up front. `pr log` renders an "in merge queue" state for PRs already queued. See [issue #103](https://github.com/mrjones2014/jj-gh/issues/103).

## Rerunning CI

`jj pr rerun <targets>` retries CI once. Targets are PR numbers and/or revsets; `--all` selects every local PR. Flags compose per PR:

| Flag                  | Effect                                                                                                                                                  |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| (default)             | Retry failed jobs on completed workflow runs. Runs still in progress are skipped with a note — GitHub does not allow retrying a run until it completes. |
| `--cancel`            | Cancel in-progress runs that already contain a failed job, wait for them to finalize (default `--cancel-timeout 30` seconds), then retry.               |
| `--all-jobs`          | Fully restart every completed run, including green ones. Distinct from `--all`, which targets all local PRs.                                            |
| `--cancel --all-jobs` | Cancel **all** in-progress runs and restart the entire pipeline.                                                                                        |

```sh
jj pr rerun 1234
jj pr rerun qn::ps --cancel
jj pr rerun --all --all-jobs
```

As with auto-merge, a failure on one PR does not stop the rest; a summary error is reported at the end. If nothing was retried because runs were still in progress, the command tells you to pass `--cancel`.

## Watch mode

`jj pr rerun --watch` keeps watching and retries retryable failures as they happen instead of exiting:

- A bare `--watch` implies `--all` and, unlike one-shot `--all`, also watches currently-green PRs — they can go red after a new push. PR numbers/revsets narrow the selection; new matches are picked up while running. Merged and closed PRs stay listed with a checkmark.
- `--poll-interval <SECS>` (default 15) controls polling; `--max-retries <N>` caps automatic retries per workflow run (unlimited by default), after which a run is marked given-up until forced.
- On a terminal it renders an inline list: `j`/`k`/`g`/`G` move, `p` pauses retrying the selected PR, `R` forces a cancel-and-retry, and `q`/Esc/Ctrl-C quits. Without a terminal it prints one plain log line per event and runs until killed.
- `--cancel` and `--all-jobs` work in watch mode too, but cancellation never blocks a poll: a cancelled run is retried on a later tick. Forced (`R`) retries bypass the retry budget and any pause.
