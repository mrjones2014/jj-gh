# jj-gh

`jj` tools for working with GitHub from your terminal.

- Create PRs locally from your preferred editor, for any arbitrary revision ID
  - Intelligently supports stacked PRs by choosing the correct base if the revision has an ancestor bookmark for which an open PR exists
- Enable auto-merge for a PR by its revision ID, without having to know/find its PR number (e.g. `jj pr auto-merge zqxy`)
- Create local bookmarks for PRs, including across forks (e.g. `jj pr fetch 1234 && jj new pr-1234/...`, useful for testing PRs to OSS repos)
- Show PR metadata like number and CI status in commit graph (e.g. `jj pr log`)
- Interactively re-stack PRs (update PR base branch based on local revision graph shape)

all from the comfort of your terminal, without touching GitHub's clunky web UI.
Works great when combined with the [jj megamerge](https://isaaccorbrey.com/notes/jujutsu-megamerges-for-fun-and-profit) workflow!

See [DOCS.md](./DOCS.md) for all commands, flags, and features. PRs welcome and encouraged!

| Writing up a PR in Neovim                                                                                  | PR number and GitHub Actions status in commit log graph                                                                   |
| ---------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| ![writing a PR in Neovim](https://github.com/user-attachments/assets/41efbe41-6b68-4a92-b0c2-776ac129dca8) | ![PR number and CI status in commit log](https://github.com/user-attachments/assets/e52826cf-2924-460c-a608-b2ccd0e7a2a7) |

## Requirements

`jj` must be on `PATH`. `pr fetch` additionally requires a colocated git repo
and `git` on `PATH` (`jj` cannot yet fetch arbitrary refs like
`refs/pull/123/head`, so the fetch step shells out to `git`).

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
  imports = [ jj-gh.homeManagerModules.default ];
  programs.jujutsu.gh = {
    enable = true;
    # Map of `jj` alias name -> `jj-gh` subcommand. Each entry installs the
    # alias *and* drops a completion overlay for `jj <name> <tab>` into any
    # shell home-manager has enabled (fish/bash/zsh).
    # aliases = { pr = "pr"; };
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

Not required, but you may also opt-in to using our Cachix binary cache:

URL: <https://jj-gh.cachix.org>

Public Key: `jj-gh.cachix.org-1:N1uFBMDd9znlhDa68BRqLSXYzXXJ2+WHVuwxpGxCtDo=`

</details>

<details>

  <summary>Precompiled binary</summary>

Download and extract the archive for your platform from the [latest GitHub release](https://github.com/mrjones2014/jj-gh/releases/latest), then
place the `jj-gh` binary somewhere in your `$PATH`.

</details>

<details>

  <summary>with <code>cargo-binstall</code></summary>

Requires [cargo-binstall](https://github.com/cargo-bins/cargo-binstall).

```sh
cargo binstall jj-gh
```

</details>

<details>

  <summary>From crates.io</summary>

Requires a Rust toolchain.

```sh
cargo install jj-gh
```

</details>

<details>

  <summary>From source</summary>

Requires a Rust toolchain. Clone this repository, then from the repo root:

```sh
cargo install --path .
```

</details>

### Setup a `jj` alias

Set up `pr` as a built-in `jj` subcommand so you can write `jj pr create <rev>`. If you use the `home-manager` module this is already done for you.

```toml
# ~/.config/jj/config.toml
[aliases]
pr = ["util", "exec", "--", "jj-gh", "pr"]
```

Now `jj pr create <rev>` (and the alias `jj pr c <rev>`) and `jj pr fetch <pr-num>` (alias `jj pr f <pr-num>`) work like any other `jj`
subcommand.

### Usage

```sh
# Show `jj log` enriched with PR metadata
jj pr log
# create a new PR from `rev-id`, supports stacked bookmarks
jj pr create rev-id
# edit an existing PR who's head ref is `rev-id`
jj pr edit rev-id
# enable auto-merge for PR who's head ref is `rev-id`;
# does not work if merge queues are enabled, this is
# a limitation in the GitHub API, see:
# https://github.com/mrjones2014/jj-gh/issues/103
jj pr auto-merge rev-id
```

#### Tips and Tricks

If you're a Neovim user you can use a plugin like [blink-cmp-git](https://github.com/Kaiser-Yang/blink-cmp-git) or
[cmp-git](https://github.com/petertriho/cmp-git) (for `nvim-cmp`) to get completions for GitHub issues and collaborators (for reviewers).

If you want to customize settings in your editor based on it being part of a `jj-gh` flow, you can use the `env` command
as part of your editor command configuration:

```toml
[jj-gh]
editor = [
  "env",
  "JJ_GH=1",
  "nvim",
  "+10",     # skip cursor past frontmatter
]
```

then check, in Neovim for example, `vim.env.JJ_GH` in Lua to make customizations specific to when you're editing PRs with `jj-gh`.

### Shell completions

`jj-gh completions <shell>` prints a standard completion script for the `jj-gh` binary. For the more common case where you
invoke `jj-gh` through a `jj` alias (e.g. `jj pr <tab>`), pass `--jj-alias <NAME> --subcommand <NAME>` (both required together)
to emit an overlay that adds completions for the alias on top of jj's own completion script.

```sh
# fish
jj util completion fish | source
jj-gh completions fish --jj-alias pr --subcommand pr | source

# bash
eval "$(jj util completion bash)"
eval "$(jj-gh completions bash --jj-alias pr --subcommand pr)"

# zsh (after compinit)
source <(jj util completion zsh)
source <(jj-gh completions zsh --jj-alias pr --subcommand pr)
```

The overlay must be sourced _after_ `jj util completion <shell>` so it can chain to jj's completer when the alias is not the one being completed.

If you use the `home-manager` module with `programs.fish.enable` / `programs.bash.enable` / `programs.zsh.enable` set, the matching overlay is
wired up automatically for every entry in `programs.jujutsu.gh.aliases`. You only need the manual steps above when installing via
`cargo install` or from source.

## Config

Add a `[jj-gh]` table to any jj config layer (global `~/.config/jj/config.toml` or repo-local config via `jj config edit --repo`).
Options related to PR metadata may also be overridden via the [markdown frontmatter](#frontmatter-format) when your editor opens.

```toml
[jj-gh]
# Auth (one source required; see "Token source precedence" below for env vars and CLI flag)
gh_askpass = ["op", "read", "op://Personal/github/token"] # preferred
gh_token = "ghp_..."                                      # plain token, less safe
askpass_timeout_secs = 20                                 # default 20

# Behavior
default_base_branch = "main" # default "master"
draft = false                # default false
auto_merge = false           # default false; enable auto-merge on PR after creation
auto_merge_method = "merge"  # default "merge"; one of "merge", "squash", "rebase"

# DEPRECATED: this is now auto-detected from the repo, will be removed in a future version
default_remote = "origin" # default remote to use

upstream_remote = "upstream" # default remote to use for cross-fork PR fetching

# PR body template. `pr_create_template` is a jj template string, evaluated
# against the revset being PR'd in chronological order. `pr_create_template_file`
# is a markdown file path. See "PR body template resolution" below for the full
# precedence list and "Template aliases" for what's available inside
# `pr_create_template`.
# Example: emit each commit's full description, separated by blank lines.
pr_create_template = 'description ++ "\n"'
# if not set, by default this will look for the following candidates:
# .github/PULL_REQUEST_TEMPLATE.md
# .github/PULL_REQUEST_TEMPLATE/PULL_REQUEST_TEMPLATE.md
# .github/pull_request_template.md
# .github/PULL_REQUEST_TEMPLATE/pull_request_template.md
pr_create_template_file = ".github/PULL_REQUEST_TEMPLATE.md"

# Bookmark name template for `pr fetch`. A jj template string, evaluated once
# against `root()` with `pr_*` aliases pre-populated from the PR's metadata.
# Default: '"pr-" ++ pr_number ++ "/" ++ pr_branch'. See "Template aliases" for
# the full list.
pr_fetch_bookmark_template = '"pr-" ++ pr_number ++ "/" ++ pr_branch'

# default template to use for `jj pr log`, a jj template string.
# the default template mimics jj's default template with PR metadata
# added inline.
pr_log_template = 'format_short_commit_header(self) ++ "#" ++ surround(" ", "", pr_number)'

# default template to be used in interactive `jj pr restack` UI;
# by default, it re-uses the `jj pr log` template, but may also be customized
# separately
pr_restack_template = 'format_short_commit_header(self) ++ "#" ++ surround(" ", "", pr_number)'

# Editor command, shell-words split. Falls back to $VISUAL, then $EDITOR.
editor = [
  "nvim",
  "+10",  # +10 jumps your cursor past the frontmatter
]

# enable or disable the use of nerdfont icons
# (e.g. in the `pr log` default template)
# NOTE: if you have issues with nerdfont icons, its most likely your `$PAGER`,
# you can fix it by either using something like `bat` (https://github.com/sharkdp/bat)
# as your pager, or setting
# ~/.config/jj/config.toml
# [ui]
# pager = { command = ["less", "-FRX"], env = { LESSCHARSET = "utf-8", LESSUTFCHARDEF = "E000-F8FF:p,F0000-FFFFD:p,100000-10FFFD:p" } }
nerdfonts = true
```

Config precedence (high to low):

1. CLI flags
1. env (`GH_ASKPASS`, `JJ_GH_TEMPLATE`, `JJ_GH_TEMPLATE_FILE`)
1. `$JJ_GH_EXTRA_CONFIG` file
1. `jj` repo-local config file
1. `jj` global config file
1. built-in defaults

`JJ_GH_TEMPLATE` maps to `pr_create_template` (jj template string).
`JJ_GH_TEMPLATE_FILE` maps to `pr_create_template_file` (path to a markdown
template).

Token source precedence (high to low):

1. `--gh-askpass` CLI flag
1. `gh_askpass` from merged config
1. `$JJ_GH_TOKEN` environment variable
1. `$GH_TOKEN` environment variable (matches the `gh` CLI convention)
1. `gh_token` from merged config (plain text, less safe)
1. Attempting to run `gh auth token`

Env vars override `gh_token` from config, but a configured `gh_askpass` still
wins. Use `$JJ_GH_TOKEN` when you need a different token for `jj-gh` than for
the `gh` CLI itself. You may also run `gh auth login` before running `jj-gh`
to use the GitHub CLI's authentication.

## Template aliases

`jj-gh` renders three different template surfaces through jj's template engine,
each with its own set of injected aliases. Aliases are pre-quoted strings, so
use them directly without wrapping in `"..."`.

### `pr create` body (`-T` / `pr_create_template`)

Evaluated against the revset being PR'd, in chronological order (`--reversed`),
so a multi-commit stack renders bottom-up. All standard jj template builtins
work (`description`, `commit_id`, `author`, etc.). Injected aliases:

- `pr_title`: default title (first-line description of the oldest commit on
  the stack).
- `pr_base`: resolved base branch; owner-qualified (`owner:branch`) for
  cross-fork PRs.
- `pr_head_branch`: existing local bookmark on the rev, or empty if the rev
  is unpushed.
- `pr_oldest_rev_id`: 40-char hex commit SHA of the oldest commit in the
  revset. Because the template runs once per commit, static content like a
  fixed PR header would otherwise be duplicated N times for an N-commit
  stack. Comparing `commit_id.short(40) == pr_oldest_rev_id` lets the
  template emit such content exactly once, at the bottom-most commit (which
  lands at the top of the output thanks to `--reversed`). Example:

```jjtemplate
if(commit_id.short(40) == pr_oldest_rev_id,
  "Fixes \n\n",
  ""
) ++ "- `" ++ description.first_line() ++ "`\n"
```

The rendered output seeds the buffer your editor opens; you can still edit
the body and frontmatter before the PR is submitted.

### `pr fetch` bookmark name (`-T` / `pr_fetch_bookmark_template`)

Evaluated once against `root()` (no commit context). Injected aliases:

- `pr_number`: PR number as a decimal string.
- `pr_title`: PR title.
- `pr_branch`: head ref name (the source branch on the PR's fork).
- `pr_url`: PR's `html_url`.
- `pr_head_sha`: 40-char hex commit SHA of the PR's head.
- `pr_head_user`: PR's head fork owner login, or empty if the fork was
  deleted.
- `pr_head_repo`: PR's head fork repository name, or empty if the fork was
  deleted.
- `pr_slug`: sanitized lowercase ASCII slug of the title (max 50 chars),
  suitable for embedding in a bookmark name.

### `pr log` (`-T` forwarded to `jj log`)

Per-commit aliases, each keyed on `commit_id` and empty for commits without
a matching open PR:

- `pr_number`: PR number as a string.
- `pr_url`: PR URL.
- `pr_ci_status`: `SUCCESS`, `FAILED`, or `PENDING`.
- `pr_merge_status`: merged / in-merge-queue / auto-merge label.
- `pr_meta`: pre-formatted hyperlinked PR number, colored CI icon, and merge
  status.

### `pr restack`

Same aliases available as `pr log`. By default, `pr restack` uses the same template as `pr log`.

## PR body template resolution

`pr create` picks the body template from the following sources, highest first:

1. `--no-template` flag (skip templating entirely).
2. `-T` / `--template` CLI (jj template string).
3. `--template-file` CLI (path).
4. Repo-layer `pr_create_template` (jj template string from repo, workspace,
   or `$JJ_GH_EXTRA_CONFIG` config).
5. Repo-layer `pr_create_template_file` (path).
6. Auto-detected `.github/PULL_REQUEST_TEMPLATE.md` (case variants included).
7. User-layer `pr_create_template` (from your global jj config).
8. User-layer `pr_create_template_file` (path from your global jj config).

The split between repo and user layers lets you set a global default jj
template while still picking up per-repo `.github/PULL_REQUEST_TEMPLATE.md`
files when contributing to OSS.

## Frontmatter format

```yaml
title: "" # required, non-empty
base: "main" # required; pre-filled with resolved base branch, or "owner:main" for cross-fork PRs
labels: [] # list of strings, applied via a follow-up API call after creation
draft: false # bool
auto_merge: false # bool; enable GitHub auto-merge once required checks pass
# this value is not present by default but may be set here as well
auto_merge_method: "merge" # one of "merge", "squash", "rebase"
```

## GitHub token permissions

I recommend using either classic or OAuth tokens, as certain functionality is not possible with fine-grained tokens due to certain
data behing behind permissions that do not exist for fine-grained tokens.

**OAuth token:**

Login with the browser flow, then no further configuration is needed; `jj-gh` will check `gh auth token` for a token source by default.

```sh
gh auth login
```

**Classic personal access token:**

- Private repos: `repo` (full control).
- Public repos only: `public_repo` is sufficient for `pr create` and `pr fetch`.

**Fine-grained personal access token**, with access to the target repositories:

Note that some functionality may not work with fine-grained tokens. Some data in the GitHub API are gated by permissions which
are not possible to grant to fine-grained tokens. See: [#167](https://github.com/mrjones2014/jj-gh/issues/167) for more information.

| Permission      | Level          | Used by                                                                                         |
| --------------- | -------------- | ----------------------------------------------------------------------------------------------- |
| Metadata        | Read           | every API call (always required)                                                                |
| Commit Statuses | Read           | Used to show GitHub Actions status for PRs in `jj pr log`                                       |
| Contents        | Read           | `pr create` (resolving the base branch ref), `pr fetch` (fetching `refs/pull/<n>/head` via git) |
| Pull requests   | Read and write | `pr create` (list + create, enable auto-merge), `pr fetch` (get)                                |
| Issues          | Read and write | `pr create` when applying labels (GitHub labels go through the Issues API)                      |

If you don't apply labels, you can drop the Issues permission. PRs are treated as Issues for the purposes of applying labels in GitHub's API.

## Output and logging

All log output goes to `STDERR`; the final PR URL (or any value the command prints) goes to `STDOUT`. Pipe-friendly:

```sh
URL=$(jj pr create zxi)
echo "Opened $URL"
```

- TTY on `STDOUT`: default log level is `INFO`.
- Piped `STDOUT`: default log level drops to `ERROR`, so only failures appear on `STDERR`.
- Override with `-v` / `-vv`, `-q`, `--log-level <level>`, or `$JJ_GH_LOG`.
