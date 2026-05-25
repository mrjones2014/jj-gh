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
    # this will set up some jj aliases like
    # pr = ["util", "exec", "--", "jj-gh", "pr", "--"]
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

## Config

Add a `[jj-gh]` table to any jj config layer (global `~/.config/jj/config.toml` or repo-local config via `jj config edit --repo`).
Options related to PR metadata may also be overidden via the [markdown frontmatter](#frontmatter-format) when your editor opens.

```toml
[jj-gh]
# Auth (one of these is required)
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
  "+9",   # +9 jumps your cursor past the frontmatter
]

# enable or disable the use of nerdfont icons
# (e.g. in the `pr log` default template)
nerdfonts = true
```

Precedence (low to high):

1. built-in defaults
1. `jj` global config file
1. `jj` repo-local config file
1. `$JJ_GH_EXTRA_CONFIG` file
1. env (`GH_ASKPASS`, `JJ_GH_TEMPLATE`)
1. CLI flags.

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

The token supplied via `gh_askpass` or `gh_token` needs different scopes depending on which subcommands you use.

**Classic personal access token:**

- Private repos: `repo` (full control).
- Public repos only: `public_repo` is sufficient for `pr create` and `pr fetch`.

**Fine-grained personal access token** (preferred), with access to the target repositories:

| Permission    | Level          | Used by                                                                                         |
| ------------- | -------------- | ----------------------------------------------------------------------------------------------- |
| Metadata      | Read           | every API call (always required)                                                                |
| Contents      | Read           | `pr create` (resolving the base branch ref), `pr fetch` (fetching `refs/pull/<n>/head` via git) |
| Pull requests | Read and write | `pr create` (list + create, enable auto-merge), `pr fetch` (get)                                |
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
