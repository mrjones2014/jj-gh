# Templates

`jj-gh` renders several surfaces through jj's template engine. Injected aliases are **pre-quoted strings**: use them directly (`pr_number`) rather than wrapping them in `"..."`. Standard jj template builtins (`description`, `commit_id`, `author`, …) are available on every surface.

## `pr create` body

`-T` / `--template` takes a jj template string, evaluated against the revset being PR'd in chronological order (`--reversed`), so a multi-commit stack renders bottom-up. Injected aliases:

| Alias              | Value                                                                                                     |
| ------------------ | --------------------------------------------------------------------------------------------------------- |
| `pr_title`         | Default title: first-line description of the oldest or newest commit, depending on `default_title_source` |
| `pr_base`          | Resolved base branch; `owner:branch` for cross-fork PRs                                                   |
| `pr_head_branch`   | Existing local bookmark on the rev, or empty if unpushed                                                  |
| `pr_oldest_rev_id` | 40-char hex SHA of the oldest commit in the revset                                                        |

Because the body template runs once per commit, static content would otherwise be duplicated N times for an N-commit stack. `pr_oldest_rev_id` lets you emit it exactly once, at the bottom-most commit (which lands at the top of the output thanks to `--reversed`):

```jjtemplate
if(commit_id.short(40) == pr_oldest_rev_id,
  "Fixes \n\n",
  ""
) ++ "- `" ++ description.first_line() ++ "`\n"
```

A markdown file can be used instead via `--template-file` or `pr_create_template_file`; a bare name is resolved under `.github/PULL_REQUEST_TEMPLATE/` (with `.md` added when missing), while absolute paths and `./`, `../`, or `~` paths are used verbatim. The rendered output seeds the editor buffer; you can still edit it before submission.

### Body template resolution

Highest source first:

1. `--no-template` (skip templating entirely)
2. `-T` / `--template` (jj template string)
3. `--template-file` (path)
4. Repo-layer `pr_create_template` — from repo config, workspace config, or `$JJ_GH_EXTRA_CONFIG`
5. Repo-layer `pr_create_template_file`
6. Auto-detected `.github/PULL_REQUEST_TEMPLATE.md`, `.github/PULL_REQUEST_TEMPLATE/PULL_REQUEST_TEMPLATE.md`, `.github/pull_request_template.md`, or `.github/PULL_REQUEST_TEMPLATE/pull_request_template.md`
7. User-layer `pr_create_template`
8. User-layer `pr_create_template_file`

The repo/workspace/extra-config split means a per-repo `.github/PULL_REQUEST_TEMPLATE.md` beats a global jj template string, while an explicit repo-local `pr_create_template` still wins over the detected file.

## `pr create` title

`--title-template` / `pr_create_title_template` (default `description.first_line()`) is evaluated once per commit in the PR revset. By default the oldest commit supplies the title; set `--title-source head` or `default_title_source = "head"` in `[jj-gh]` to use the newest. `--pick-title` opens a picker showing each short change ID alongside its rendered title. Titles must be non-empty and single-line. The chosen default is also what `pr_title` expands to inside the body template.

## `pr fetch` bookmark name

The bookmark template is evaluated once against `root()` (no commit context). Default: `"pr-" ++ pr_number ++ "/" ++ pr_branch`.

| Alias          | Value                                                       |
| -------------- | ----------------------------------------------------------- |
| `pr_number`    | PR number as a decimal string                               |
| `pr_title`     | PR title                                                    |
| `pr_branch`    | Head ref name (source branch on the PR's fork)              |
| `pr_url`       | PR URL (`html_url`)                                         |
| `pr_head_sha`  | 40-char hex SHA of the PR head                              |
| `pr_head_user` | Head fork owner login, or empty if the fork was deleted     |
| `pr_head_repo` | Head fork repository name, or empty if the fork was deleted |
| `pr_slug`      | Sanitized lowercase ASCII slug of the title, max 50 chars   |

```sh
jj pr fetch 1234 -T '"pr-" ++ pr_number ++ "-" ++ pr_slug'
```

See [Fetch](./workflows/fetch.md) for cross-fork usage and the force flag.

## `pr log` and `pr stack`

`pr log` injects these per-commit aliases (empty for commits without an open PR); `pr stack` uses the same aliases and prints the log before its plan:

| Alias             | Value                                                                  |
| ----------------- | ---------------------------------------------------------------------- |
| `pr_number`       | PR number as a string                                                  |
| `pr_url`          | PR URL                                                                 |
| `pr_ci_status`    | `SUCCESS`, `FAILED`, or `PENDING`                                      |
| `pr_merge_status` | merged / in-merge-queue / auto-merge label                             |
| `pr_meta`         | Pre-formatted hyperlinked PR number, colored CI icon, and merge status |

The injected `pr_log` template (configurable via `pr_log_template`) uses `pr_meta` so spacing appears only for commits with a PR. For `pr log`, pass your own template after `--` to replace it entirely; a `-T` before `--` only changes the injected template's body. `pr stack` accepts `-T` for the same log view plus `--nerdfonts` / `--no-nerdfonts`. See [Log](./workflows/log.md).
