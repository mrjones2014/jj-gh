# Fetching a PR

`jj pr fetch <pr-num>` (alias `jj pr f`) fetches a pull request's head into a local bookmark and imports it into jj:

```sh
jj pr fetch 1234
jj new pr-1234/feature-branch
```

The argument is a **PR number only** — there is no revision-ID lookup path. `jj-gh` reads the PR metadata from GitHub's API, fetches GitHub's special `refs/pull/<n>/head` ref into the local bookmark using [gix](https://github.com/GitoxideLabs/gitoxide), then runs `jj git import`. gix is used because `jj` cannot yet fetch arbitrary refs (only `refs/heads/*`). No `.git` directory at the workspace root is required; the git store is located via `jj git root`.

## Cross-fork PRs

`refs/pull/<n>/head` lives on the repository that hosts the PR, which in a fork workflow is the upstream, not your fork. The host remote is resolved in this order:

1. `--upstream-remote` flag
2. `--remote` flag
3. `upstream_remote` config (used only if that remote exists)
4. git's auto-detected default push remote
5. `default_remote` config (a fallback, not deprecated)

Explicit flags naming an unconfigured remote fail loudly. The target owner/repo for the API call is derived from the selected remote's URL, so `jj pr fetch 1234 --upstream-remote upstream` works even when your own default remote is a fork.

## Bookmark name

The name is rendered by a jj template evaluated once against `root()`, with these injected aliases (pre-quoted strings — use them directly):

| Alias          | Value                                                       |
| -------------- | ----------------------------------------------------------- |
| `pr_number`    | PR number as a decimal string                               |
| `pr_title`     | PR title                                                    |
| `pr_branch`    | Head ref name (source branch on the PR's fork)              |
| `pr_url`       | PR URL (`html_url`)                                         |
| `pr_head_sha`  | 40-char hex SHA of the PR head                              |
| `pr_head_user` | Head fork owner login, or empty if the fork was deleted     |
| `pr_head_repo` | Head fork repository name, or empty if the fork was deleted |
| `pr_slug`      | Lowercase ASCII slug of the title, max 50 chars             |

Default: `"pr-" ++ pr_number ++ "/" ++ pr_branch`. Override per invocation with `-T` / `--template`, or permanently with `pr_fetch_bookmark_template` in [Configuration](../configuration.md):

```sh
jj pr fetch 1234 -T '"pr-" ++ pr_number ++ "-" ++ pr_slug'
jj pr fetch 1234 -T '"review/" ++ pr_head_user ++ "/" ++ pr_branch'
```

An existing local bookmark with the same name is an error unless `-f` / `--force` is passed; `--force` replaces it. The rendered bookmark name is printed on `STDOUT`, so `BOOKMARK=$(jj pr fetch 1234)` works in scripts. See [Templates](../templates.md) for more on the alias surfaces.
