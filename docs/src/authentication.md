# Authentication

`jj-gh` needs a GitHub token to read and create PRs. Tokens are handled as secrets: they are never logged, and `jj-gh debug auth` reports whether a token resolves without printing it.

## Recommended token types

Use an **OAuth token** or a **classic personal access token**. Some GitHub API data used by `jj-gh` is gated behind permissions that fine-grained tokens cannot request, so some features may not work with fine-grained tokens. See [issue #167](https://github.com/mrjones2014/jj-gh/issues/167).

**OAuth token (simplest):** log in with the browser flow, then no further configuration is needed. `jj-gh` falls back to `gh auth token` when no higher-priority source is set.

```sh
gh auth login
```

**Classic personal access token:**

| Repository access | Scope needed                                               |
| ----------------- | ---------------------------------------------------------- |
| Private repos     | `repo` (full control)                                      |
| Public repos only | `public_repo` is sufficient for `pr create` and `pr fetch` |

**Fine-grained personal access token**, with access to the target repositories:

| Permission      | Level          | Used by                                                                                         |
| --------------- | -------------- | ----------------------------------------------------------------------------------------------- |
| Metadata        | Read           | every API call (always required)                                                                |
| Commit Statuses | Read           | Used to show GitHub Actions status for PRs in `jj pr log`                                       |
| Contents        | Read           | `pr create` (resolving the base branch ref), `pr fetch` (fetching `refs/pull/<n>/head` via gix) |
| Pull requests   | Read and write | `pr create` (list + create, enable auto-merge), `pr fetch` (get)                                |
| Issues          | Read and write | `pr create` when applying labels (GitHub labels go through the Issues API)                      |

If you do not apply labels, you can drop the Issues permission. GitHub treats PRs as Issues for the purposes of applying labels.

## Token source precedence

`jj-gh` uses the first source that yields a token, from highest to lowest priority:

| Priority | Source                 | Notes                                                                      |
| -------- | ---------------------- | -------------------------------------------------------------------------- |
| 1        | `--gh-askpass <CMD>`   | CLI flag; command prints the token on stdout. Outranks everything else.    |
| 2        | `$GH_ASKPASS`          | Environment variable containing an askpass command; shell-split and run.   |
| 3        | `$JJ_GH_TOKEN`         | Use this when `jj-gh` needs a different token than the `gh` CLI.           |
| 4        | `$GH_TOKEN`            | Matches the `gh` CLI convention.                                           |
| 5        | `gh_askpass` in config | Askpass command from any `[jj-gh]` config layer.                           |
| 6        | `gh_token` in config   | Plain text token; less safe than an askpass helper.                        |
| 7        | `gh auth token`        | Falls back to the GitHub CLI, so `gh auth login` is enough to get started. |

Flags and environment variables always override `gh_askpass`/`gh_token` set in config files. An askpass helper must exit successfully and print exactly one token to stdout; helpers time out after `askpass_timeout_secs` (default `20`, override with `--askpass-timeout`).

## Examples

Use the `gh` CLI's authentication:

```sh
gh auth login
```

Use an environment token (one-off or in your shell profile):

```sh
export JJ_GH_TOKEN='<token>'   # jj-gh only
export GH_TOKEN='<token>'      # follows the gh convention
```

Use an askpass helper from the environment or CLI:

```sh
export GH_ASKPASS='op read op://Vault/GitHub/token'
jj pr log --gh-askpass 'op read op://Vault/GitHub/token'
```

Configure a helper (or a plain token) in any jj config layer:

```toml
[jj-gh]
gh_askpass = ["op", "read", "op://Vault/GitHub/token"]  # preferred
# gh_token = "<token>"  # plain text, less safe
askpass_timeout_secs = 20
```

See [Configuration](./configuration.md) for where `[jj-gh]` settings can live.
