# Configuration

Add a `[jj-gh]` table to any jj config layer (global `~/.config/jj/config.toml`, repo-local via `jj config edit --repo`, or a workspace config). jj-gh reads the `jj-gh` subtree through `jj config list`, so jj's own discovery rules (XDG dirs, `conf.d` includes, `--config` overrides) apply.

```toml
[jj-gh]
default_base_branch = "master"
default_title_source = "head"
pr_fetch_bookmark_template = '"pr-" ++ pr_number ++ "-" ++ pr_slug'
nerdfonts = false
```

Use `jj-gh debug config` to inspect the merged config (the token is rendered as `***`).

## Settings

### Authentication

| Key                    | Type                       | Default | Description                                                                                                                 |
| ---------------------- | -------------------------- | ------- | --------------------------------------------------------------------------------------------------------------------------- |
| `gh_askpass`           | command (array of strings) | unset   | Helper that prints a GitHub token on stdout. Prefer this over a plain token. Outranked by `--gh-askpass` and `$GH_ASKPASS`. |
| `askpass_timeout_secs` | integer                    | `20`    | Seconds allowed for an askpass helper.                                                                                      |
| `gh_token`             | string                     | unset   | Plain token; less safe. Never logged.                                                                                       |

Token source precedence (high to low), resolved outside the config layering:

1. `--gh-askpass` CLI flag
2. `$GH_ASKPASS` environment variable (shell-split command)
3. `$JJ_GH_TOKEN` environment variable
4. `$GH_TOKEN` environment variable (matches the `gh` CLI)
5. `gh_askpass` from config
6. `gh_token` from config
7. `gh auth token` (run `gh auth login` first to use this)

`$JJ_GH_TOKEN` is useful when jj-gh needs a different token than the `gh` CLI. See [Authentication](./authentication.md).

### Behavior

| Key                   | Type                        | Default                                | Description                                                                                                                                                           |
| --------------------- | --------------------------- | -------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `default_base_branch` | string or unset             | unset (`None`)                         | Fallback base only when neither `--base`, an ancestor bookmark, nor jj `trunk()` resolves. If still unresolved, the command errors.                                   |
| `default_remote`      | string or unset             | unset (`None`)                         | **Not deprecated.** Fallback for your own pushes and PR head lookups, used only when git's default remote cannot be auto-resolved; `--remote` and auto-detection win. |
| `upstream_remote`     | string or unset             | unset (`None`), effectively `upstream` | Git remote used as the PR target in fork workflows and as the PR host for `fetch`. Falls back to `upstream` when unset.                                               |
| `draft`               | bool                        | `false`                                | Open new PRs as drafts.                                                                                                                                               |
| `auto_merge`          | mode                        | `false`                                | Auto-merge scope for new PRs: `false`, `infer`, `all`, or `below`. See [Auto-merge and rerun](./workflows/auto-merge-rerun.md).                                       |
| `auto_merge_method`   | `merge`, `squash`, `rebase` | `merge`                                | Merge method when auto-merge is enabled.                                                                                                                              |
| `auto_stack`          | bool                        | `true`                                 | Link created stacked PRs into a GitHub stack.                                                                                                                         |
| `auto_push`           | bool                        | `true`                                 | Push stale bookmarks before `pr stack` links PRs into a stack.                                                                                                        |

`auto_merge` accepts `false` (off, the default), `infer` (whole stack when the PR is stacked, otherwise just that PR), `all` (whole GitHub stack through its top PR), and `below` (an individual request for this PR, without stack-wide coverage). Boolean `auto_merge = true` is deprecated and treated as `infer`. See [Auto-merge and rerun](./workflows/auto-merge-rerun.md) for the CLI flags and creation-time behavior.

### Templates and editor

| Key                          | Type                                | Default                                  | Description                                                                                                                                                    |
| ---------------------------- | ----------------------------------- | ---------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `pr_create_template`         | jj template string or unset         | unset                                    | Body template; also settable via `$JJ_GH_TEMPLATE`. Aliases are injected; see [Templates](./templates.md).                                                     |
| `pr_create_template_file`    | path or unset                       | unset                                    | Markdown body template file; also settable via `$JJ_GH_TEMPLATE_FILE`.                                                                                         |
| `pr_create_title_template`   | jj template string                  | `description.first_line()`               | Renders candidate PR titles (once per commit).                                                                                                                 |
| `default_title_source`       | `base` or `head`                    | `base`                                   | Which commit supplies the default title: oldest (`base`) or newest (`head`). Overridden by `--title-source`.                                                   |
| `pr_fetch_bookmark_template` | jj template string or unset         | `"pr-" ++ pr_number ++ "/" ++ pr_branch` | Bookmark name template for `pr fetch`.                                                                                                                         |
| `pr_log_template`            | jj template string or unset         | built-in PR-aware template               | Body of the injected `pr_log` template used by `pr log` (and the `pr stack` log view).                                                                         |
| `editor`                     | command (array of strings) or unset | unset                                    | Editor for the frontmatter flow. Falls back to jj's `ui.editor`, which jj resolves from `$JJ_EDITOR`, `$VISUAL`, `$EDITOR`. `--editor` wins over all of these. |

### Diff previews and icons

| Key                    | Type | Default | Description                                                                                                          |
| ---------------------- | ---- | ------- | -------------------------------------------------------------------------------------------------------------------- |
| `pr_create_show_diffs` | bool | `true`  | Show the read-only diff preview while creating a PR body. Overridden by `--diffs` / `--no-diffs`.                    |
| `pr_edit_show_diffs`   | bool | `true`  | Show the diff preview while editing a PR. Overridden by `--diffs` / `--no-diffs`.                                    |
| `nerdfonts`            | bool | `true`  | Use Nerd-Font glyphs in the default `pr log` / `pr stack` templates. Overridden by `--nerdfonts` / `--no-nerdfonts`. |

## Precedence

Config assembly uses these layers, lowest priority first:

1. Built-in defaults
2. jj's `ui.editor` (only maps to the `editor` key)
3. jj user config (`jj config list --user jj-gh`)
4. jj repo config (`jj config list --repo jj-gh`)
5. jj workspace config (`jj config list --workspace jj-gh`)
6. The file named by `$JJ_GH_EXTRA_CONFIG`
7. Environment overlay (`JJ_GH_TEMPLATE` → `pr_create_template`, `JJ_GH_TEMPLATE_FILE` → `pr_create_template_file`)
8. CLI flags

So, high to low: **CLI flags → env → `$JJ_GH_EXTRA_CONFIG` → workspace → repo → user → jj `ui.editor` → built-ins**. The main merged config uses jj's unscoped `jj config list jj-gh`; the scoped queries exist for the per-layer body-template split below.

`pr create` body resolution is deliberately split by layer (highest first):

1. `--no-template`
2. `-T` / `--template` (jj template string)
3. `--template-file` (path)
4. Repo-layer `pr_create_template` (repo, workspace, or `$JJ_GH_EXTRA_CONFIG`)
5. Repo-layer `pr_create_template_file`
6. Auto-detected `.github/PULL_REQUEST_TEMPLATE.md` (and case variants)
7. User-layer `pr_create_template`
8. User-layer `pr_create_template_file`

This lets you keep a global default while still picking up each OSS repo's own pull request template. Full details are in [Templates](./templates.md).

## JSON schema

`jj-gh config schema` prints the JSON Schema for the `[jj-gh]` table on stdout. Point your editor's JSON-schema support at it to get completion and validation while editing jj config:

```sh
jj-gh config schema > ~/.config/jj/jj-gh.schema.json
```
