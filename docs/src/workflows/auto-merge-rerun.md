# Auto-merge and CI reruns

## Auto-merge

`jj pr auto-merge <targets>` (alias `jj pr am`) enables auto-merge on one or more PRs. Targets may be PR numbers and/or jj revsets; a revset can match a whole stack (for example `qv::uw`) and is resolved through the commits' local bookmarks, bottom to top.

```sh
jj pr auto-merge 1234
jj pr auto-merge zqxy                 # revision with a bookmarked PR
jj pr auto-merge qv::uw --method squash
```

`-m` / `--method` chooses `merge` (default), `squash`, or `rebase`, overriding `auto_merge_method` from [Configuration](../configuration.md). The same method is available at PR creation via `auto_merge = true` / `--auto-merge` and `--auto-merge-method`.

- **Merge queues:** auto-merge cannot be enabled in repositories using GitHub merge queues — a GitHub API limitation (see [issue #103](https://github.com/mrjones2014/jj-gh/issues/103)). `pr log` will show an "in merge queue" state instead.
- **Multiple targets:** a failure on one PR does not stop the others. Each failing PR is logged, and the command exits with a summary error such as `failed to enable auto-merge on 1 of 3 PR(s)`. If only one PR was targeted, its real error is reported directly.

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
