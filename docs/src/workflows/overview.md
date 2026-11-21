# Everyday workflows

With the `pr` alias installed (`jj pr` → `jj-gh pr`), these are the commands you'll use most. See [Installation](../installation.md) if you haven't set the alias up yet.

| Command                      | What it does                                                                                                                                  |
| ---------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| `jj pr log`                  | `jj log` with PR number, CI status, and merge state inline. See [Log](./log.md).                                                              |
| `jj pr create <rev>`         | Open your editor to write a PR from a revision; supports stacked PRs. The editor frontmatter and body are shared with [Edit](./edit.md).      |
| `jj pr edit <pr-num\|rev>`   | Edit an existing PR's title, body, base, labels, reviewers, draft state, and auto-merge via the same frontmatter flow. See [Edit](./edit.md). |
| `jj pr fetch <pr-num>`       | Fetch a PR head into a local bookmark, including across forks. See [Fetch](./fetch.md).                                                       |
| `jj pr auto-merge <targets>` | Enable auto-merge by PR number or revset. See [Auto-merge and rerun](./auto-merge-rerun.md).                                                  |
| `jj pr rerun <targets>`      | Retry failed CI jobs, once or continuously with `--watch`. See [Auto-merge and rerun](./auto-merge-rerun.md).                                 |
| `jj pr stack`                | Make GitHub match the shape of your local `jj` graph; see [Reconciling stacks](../stack-reconciliation.md).                                   |
| `jj pr url <pr-num\|rev>`    | Print a PR's full URL. Handy in pipes: `jj pr url <rev> \| pbcopy` (or `wl-copy`).                                                            |

Notes that apply across these commands:

- Log output goes to `STDERR`; the command's final value (for example a PR URL or bookmark name) goes to `STDOUT`, so pipes stay clean.
- Most commands accept PR numbers and/or jj revsets; a numeric argument is a PR number, anything else is resolved as a revset. A revset is looked up through the commits' local bookmarks.
- Remote selection is shared: `--remote` for your own pushes/head lookups and `--upstream-remote` for the PR target in fork workflows. See [Configuration](../configuration.md) for the config keys and precedence.
