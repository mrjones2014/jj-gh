# jj-gh

`jj` tools for working with GitHub from your terminal.

- Create PRs locally from your preferred editor, for any arbitrary revision ID
  - Intelligently supports stacked PRs by choosing the correct base if the revision has an ancestor bookmark for which an open PR exists
- Enable auto-merge for a PR by its revision ID, without having to know/find its PR number (e.g. `jj pr auto-merge zqxy`)
- Create local bookmarks for PRs, including across forks (e.g. `jj pr fetch 1234 && jj new pr-1234/...`, useful for testing PRs to OSS repos)
- Show PR metadata like number and CI status in commit graph (e.g. `jj pr log`)

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

### Shell completions

`jj-gh completions <shell>` prints a standard completion script for the `jj-gh` binary. For the more common case where you invoke `jj-gh` through a `jj` alias (e.g. `jj pr <tab>`), pass `--jj-alias <NAME> --subcommand <NAME>` (both required together) to emit an overlay that adds completions for the alias on top of jj's own completion script.

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

If you use the `home-manager` module with `programs.fish.enable` / `programs.bash.enable` / `programs.zsh.enable` set, the matching overlay is wired up automatically for every entry in `programs.jujutsu.gh.aliases`. You only need the manual steps above when installing via `cargo install` or from source.

## Config

Add a `[jj-gh]` table to any jj config layer (global `~/.config/jj/config.toml` or repo-local config via `jj config edit --repo`).
Options related to PR metadata may also be overidden via the [markdown frontmatter](#frontmatter-format) when your editor opens.

```toml
[jj-gh]
# Auth (one source required; see "Token source precedence" below for env vars and CLI flag)
gh_askpass = ["op", "read", "op://Personal/github/token"] # preferred
gh_token = "ghp_..."                                      # plain token, less safe
askpass_timeout_secs = 20                                 # default 20

# Behavior
default_base_branch = "main"                       # default "master"
template_path = ".github/PULL_REQUEST_TEMPLATE.md"
draft = false                                      # default false
auto_merge = false                                 # default false; enable auto-merge on PR after creation
auto_merge_method = "merge"                        # default "merge"; one of "merge", "squash", "rebase"

# Bookmark name template for `pr fetch`. Default "pr-{number}/{branch}".
# Placeholders: {number}, {branch}, {user}, {repo}. `{{` / `}}` are literal.
pr_fetch_bookmark_template = "pr-{number}/{branch}"

# Editor command, shell-words split. Falls back to $VISUAL, then $EDITOR.
editor = [
  "nvim",
  "+10",  # +10 jumps your cursor past the frontmatter
]

# enable or disable the use of nerdfont icons
# (e.g. in the `pr log` default template)
nerdfonts = true
```

Config precedence (high to low):

1. CLI flags
1. env (`GH_ASKPASS`, `JJ_GH_TEMPLATE`)
1. `$JJ_GH_EXTRA_CONFIG` file
1. `jj` repo-local config file
1. `jj` global config file
1. built-in defaults

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

## Frontmatter format

```yaml
title: "" # required, non-empty
base: "main" # required; pre-filled with the resolved base branch
labels: [] # list of strings, applied via a follow-up API call after creation
draft: false # bool
auto_merge: false # bool; enable GitHub auto-merge once required checks pass
# this value is not present by default but may be set here as well
auto_merge_method: "merge" # one of "merge", "squash", "rebase"
```

## GitHub token permissions

The token supplied via `gh_askpass`, `gh_token`, `$JJ_GH_TOKEN`, or `$GH_TOKEN` needs a few permissions to function.

**Fine-grained personal access token** (preferred), with access to the target repositories:

| Permission    | Level          | Used by                                                                                         |
| ------------- | -------------- | ----------------------------------------------------------------------------------------------- |
| Metadata      | Read           | every API call (always required)                                                                |
| Contents      | Read           | `pr create` (resolving the base branch ref), `pr fetch` (fetching `refs/pull/<n>/head` via git) |
| Pull requests | Read and write | `pr create` (list + create, enable auto-merge), `pr fetch` (get)                                |
| Issues        | Read and write | `pr create` when applying labels (GitHub labels go through the Issues API)                      |

**Classic personal access token:**

- Private repos: `repo` (full control).
- Public repos only: `public_repo` is sufficient for `pr create` and `pr fetch`.

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
