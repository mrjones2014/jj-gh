# jj-gh

Opinionated `jj` tools for working with GitHub from your terminal.

## Requirements

`jj` must be on `PATH`.

## Install

Nix flake:

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

From source:

```sh
cargo install --path .
```

Will publish to `crates.io` in the future.

### As a `jj` alias

Set up `pr` as a built-in `jj` subcommand so you can write `jj pr create <rev>`:

```toml
# ~/.config/jj/config.toml
[aliases]
pr = ["util", "exec", "--", "jj-gh", "pr"]
```

Now `jj pr create <rev>` (and the alias `jj pr c <rev>`) works like any other `jj` subcommand.

## Usage

```sh
jj pr create <rev>
```

The editor opens with a buffer like:

```markdown
---
title: "feat(thing): do the thing"
base: "main"
labels: []
draft: false
---

Body in markdown.
```

Save and quit. `jj-gh` pushes the change with `jj git push -c`, opens the PR, applies labels, and prints the URL on stdout.

If a PR is already open for the head, the existing URL is printed and nothing is changed.

### Base-branch resolution

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

### Debug subcommands

| Command                       | Purpose                                                                                       |
| ----------------------------- | --------------------------------------------------------------------------------------------- |
| `jj-gh debug config`          | Print the merged config with the token redacted.                                              |
| `jj-gh debug auth`            | Resolve the GitHub token and report success or failure. Never prints the actual token itself. |
| `jj-gh debug rev <REV>`       | Resolve a rev to commit info, remote URLs, and the detected default branch.                   |
| `jj-gh debug pr-lookup <REV>` | Pre-flight: target, existing PR (if any), base-branch existence.                              |

## Config

Add a `[tools.jj-gh]` table to any jj config layer (global `~/.config/jj/config.toml` or repo-local `.jj/repo/config.toml`):

```toml
[tools.jj-gh]
# Auth (one of these is required).
gh_askpass = ["op", "read", "op://Personal/github/token"] # preferred
gh_token = "ghp_..."                                      # plain token, less safe
askpass_timeout_secs = 20                                 # default 20

# Behavior
default_base_branch = "main"                       # default "master"
template_path = ".github/PULL_REQUEST_TEMPLATE.md"
draft = false                                      # default false

# Editor command, shell-words split. Falls back to $VISUAL, then $EDITOR.
editor = [
  "nvim",
  "+7",   # +7 jumps your cursor past the frontmatter
]
```

Precedence (low to high): built-in defaults < jj global < jj repo-local < `$JJ_GH_EXTRA_CONFIG` file < env (`GH_ASKPASS`, `JJ_GH_TEMPLATE`) < CLI flags.

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

```sh
direnv allow       # or `nix develop` if preferred
cargo nextest run  # or `cargo nt` alias, runs nextest
cargo clippy
cargo fmt --check
nix flake check    # runs everything via crane
```
