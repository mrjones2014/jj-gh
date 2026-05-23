# jj-gh

Opinionated `jj` tools for working with GitHub from your terminal.

- Create PRs from any revision, including smart support for stacked PRs
- Easily fetch PRs for local checkout, including across forks

all from the comfort of your terminal, without touching GitHub's clunky web UI.
Works great when combined with the [jj megamerge](https://isaaccorbrey.com/notes/jujutsu-megamerges-for-fun-and-profit) workflow!

PRs welcome and encouraged!

## Requirements

`jj` must be on `PATH`. `pr fetch` additionally requires a colocated git repo
and `git` on `PATH` (jj cannot yet fetch arbitrary refs like
`refs/pull/123/head`, so the fetch step shells out to git).

## Install

<details>

  <summary>With Nix</summary>

Add the flake input:

```nix
{
  inputs.jj-gh.url = "github:mrjones2014/jj-gh";
  outputs =
    {
      self,
      nixpkgs,
      jj-gh,
      ...
    }:
    {
      # use jj-gh.packages.${system}.default
    };
}
```

You can either use the overlay directly, or use the `home-manager` module.

```nix
{ jj-gh, pkgs, ... }:
{
  # overlay, not needed if using home-manager module
  nixpkgs.overlays = [ jj-gh.overlays.default ];
  home.packages = [ pkgs.jj-gh ];

  # home-manager
  imports = [ jj-gh.hmModules.default ];
  programs.jujutsu.gh = {
    # this will set up some jj aliases like
    # pr = ["util", "exec", "--", "jj-gh", "pr"]
    enable = true;
    settings = {
      gh_askpass = [
        "op"
        "read"
        "op://Private/GitHub/token"
      ];
    };
  };
}
```

</details>

<details>

  <summary>From source</summary>

Requires Rust toolchain. Will publish to `crates.io` in the future.

```sh
cargo install --path .
```

### Setup a `jj` alias

Set up `pr` as a built-in `jj` subcommand so you can write `jj pr create <rev>`:

```toml
# ~/.config/jj/config.toml
[aliases]
pr = ["util", "exec", "--", "jj-gh", "pr"]
```

Now `jj pr create <rev>` (and the alias `jj pr c <rev>`) and
`jj pr fetch <pr-num>` (alias `jj pr f <pr-num>`) work like any other `jj`
subcommand.

</details>

## Usage

### Creating a PR

```sh
jj pr create <rev>
```

The editor opens with a buffer like:

```markdown
---
title: "feat(thing): do the thing"
base: "master"
labels: []
draft: false
---

Body in markdown.
```

Save and quit. `jj-gh` pushes the change with `jj git push -c`, opens the PR, applies labels, and prints the URL on stdout.

If a PR is already open for the head, the existing URL is printed and nothing is changed.

#### Base-branch resolution

`jj-gh` supports stacked PRs by picking a smart default base:

1. `--base <branch>` if you pass one.
2. Otherwise the closest ancestor commit with a bookmark (so PR #2 stacked on PR #1's branch targets PR #1's bookmark).
3. Otherwise the bookmark at jj's `trunk()` revset (whatever the repo's `revsets.trunk` resolves to; default probes `main@<remote>`, `master@<remote>`, `trunk@<remote>`).
4. Otherwise the configured `default_base_branch` (default `master`).

### Flags

| Flag                        | Default                                              | Effect                                                                                                                                                        |
| --------------------------- | ---------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `--base <branch>`           | stacked ancestor / `trunk()` / `default_base_branch` | Override the base branch.                                                                                                                                     |
| `--draft` / `--no-draft`    | config `draft` (default `false`)                     | Force the draft state.                                                                                                                                        |
| `--template <path-or-name>` | config `template_path` / auto-detect                 | Use a specific template. Paths starting with `./`, `../`, `/`, or `~` are taken verbatim; bare names resolve under `.github/PULL_REQUEST_TEMPLATE/<name>.md`. |
| `--no-template`             | off                                                  | Skip template selection entirely.                                                                                                                             |
| `--editor <cmd>`            | config `editor` / `$VISUAL` / `$EDITOR`              | Editor command.                                                                                                                                               |
| `--gh-askpass <cmd>`        | config `gh_askpass` / `$GH_ASKPASS`                  | Askpass helper command that prints the GitHub token to stdout.                                                                                                |
| `--askpass-timeout <secs>`  | `20`                                                 | Timeout for the askpass helper.                                                                                                                               |

### Fetching a PR

```sh
jj pr fetch <pr-num>
```

Downloads `refs/pull/<pr-num>/head` from `origin` into a local bookmark and
imports it into jj. The bookmark name on stdout is pipe-friendly; the title,
head commit, PR URL, and a follow-up hint go to stderr (TTY only).

```sh
$ jj pr fetch 1234
PR #1234: Add the feature
head: abc123... (https://github.com/o/r/pull/1234)
hint: jj new pr-1234/feature/foo
pr-1234/feature/foo
```

The bookmark name comes from a template (`pr_fetch_bookmark_template` in
config, or `-t/--template` on the CLI). Default: `pr-{number}/{branch}`.

Placeholders:

- `{number}`: PR number.
- `{branch}`: `head.ref` of the PR, raw (slashes preserved).
- `{user}`: `head.user.login` (the fork owner's GitHub login).
- `{repo}`: `head.repo.name` (the head repository's name).

Use `{{` and `}}` for literal braces.

I recommend keeping a unique, recognizable prefix (the default `pr-{number}/`
form works) so you can bulk-delete stale bookmarks after a PR merges, e.g.
`jj bookmark delete 'pr-1234/*'`.

#### Requirements

`pr fetch` shells out to `git` to grab the special `refs/pull/123/head` ref
because jj cannot yet fetch arbitrary refs (only `refs/heads/*`). It therefore
requires a colocated git repository.

#### Flags

| Flag                       | Default                                                      | Effect                                                                                 |
| -------------------------- | ------------------------------------------------------------ | -------------------------------------------------------------------------------------- |
| `-t, --template <STR>`     | config `pr_fetch_bookmark_template` / `pr-{number}/{branch}` | Override the bookmark template.                                                        |
| `-f, --force`              | off                                                          | Replace an existing local bookmark of the same name (passes `--force` to `git fetch`). |
| `--gh-askpass <cmd>`       | config `gh_askpass` / `$GH_ASKPASS`                          | Askpass helper command that prints the GitHub token to stdout.                         |
| `--askpass-timeout <secs>` | `20`                                                         | Timeout for the askpass helper.                                                        |

### Debug subcommands

| Command                       | Purpose                                                                                       |
| ----------------------------- | --------------------------------------------------------------------------------------------- |
| `jj-gh debug config`          | Print the merged config with the token redacted.                                              |
| `jj-gh debug auth`            | Resolve the GitHub token and report success or failure. Never prints the actual token itself. |
| `jj-gh debug rev <REV>`       | Resolve a rev to commit info, remote URLs, and the detected default branch.                   |
| `jj-gh debug pr-lookup <REV>` | Pre-flight: target, existing PR (if any), base-branch existence.                              |

## Config

Add a `[jj-gh]` table to any jj config layer (global `~/.config/jj/config.toml` or repo-local `.jj/repo/config.toml`):

```toml
[jj-gh]
# Auth (one of these is required).
gh_askpass = ["op", "read", "op://Personal/github/token"] # preferred
gh_token = "ghp_..."                                      # plain token, less safe
askpass_timeout_secs = 20                                 # default 20

# Behavior
default_base_branch = "main"                       # default "master"
template_path = ".github/PULL_REQUEST_TEMPLATE.md"
draft = false                                      # default false

# Bookmark name template for `pr fetch`. Default "pr-{number}/{branch}".
# Placeholders: {number}, {branch}, {user}, {repo}. `{{` / `}}` are literal.
pr_fetch_bookmark_template = "pr-{number}/{branch}"

# Editor command, shell-words split. Falls back to $VISUAL, then $EDITOR.
editor = [
  "nvim",
  "+7",   # +7 jumps your cursor past the frontmatter
]
```

Precedence (low to high): built-in defaults < jj global < jj repo-local < `$JJ_GH_EXTRA_CONFIG` file < env (`GH_ASKPASS`, `JJ_GH_TEMPLATE`) < CLI flags.

### GitHub token permissions

The token supplied via `gh_askpass` or `gh_token` needs different scopes depending on which subcommands you use.

**Classic personal access token:**

- Private repos: `repo` (full control).
- Public repos only: `public_repo` is sufficient for `pr create` and `pr fetch`.

**Fine-grained personal access token** (preferred), with access to the target repositories:

| Permission    | Level          | Used by                                                                                         |
| ------------- | -------------- | ----------------------------------------------------------------------------------------------- |
| Metadata      | Read           | every API call (always required)                                                                |
| Contents      | Read           | `pr create` (resolving the base branch ref), `pr fetch` (fetching `refs/pull/<n>/head` via git) |
| Pull requests | Read and write | `pr create` (list + create), `pr fetch` (get)                                                   |
| Issues        | Read and write | `pr create` when applying labels (GitHub labels go through the Issues API)                      |

If you don't apply labels, you can drop the Issues permission.

## Output and logging

All log output goes to `STDERR`; the final PR URL (or any value the command prints) goes to `STDOUT`. Pipe-friendly:

```sh
URL=$(jj pr create zxi)
echo "Opened $URL"
```

- TTY on `STDOUT`: default log level is `INFO`.
- Piped `STDOUT`: default log level drops to `ERROR`, so only failures appear on `STDERR`.
- Override with `-v` / `-vv`, `-q`, `--log-level <level>`, or `$JJ_GH_LOG`.

## Frontmatter format

```yaml
title: "" # required, non-empty
base: "main" # required; pre-filled with the resolved base branch
labels: [] # list of strings, applied via a follow-up API call after creation
draft: false # bool
```

## Development

The only development dependency is Nix.

I recommend setting the following repo configs for `jj`:

```toml
[aliases]
pr = ["util", "exec", "--", "nix", "run", ".#", "--", "pr"]

[fix.tools.treefmt]
command = ["treefmt", "--quiet", "--stdin", "$path"]
patterns = ["glob:'**/*'"]
```

```sh
direnv allow                 # or `nix develop` if preferred
nix build                    # build the CLI with Nix caching
nix run .# -- pr create zxy  # run the cached build
nix flake check              # runs everything via crane
# running cargo directly loses out on nix caching but still useful for local dev
cargo nextest run            # or `cargo nt` alias, runs nextest
cargo clippy
cargo fmt --check
```
