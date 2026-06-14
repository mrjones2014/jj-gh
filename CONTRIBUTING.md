# Contributing

Using Nix (or [Lix](https://lix.systems/)) is recommended but not required.

<!--toc:start-->

- [Contributing](#contributing)
  - [Commits](#commits)
  - [ast-grep](#ast-grep)
  - [With Nix (recommended)](#with-nix-recommended)
    - [Optional configuration](#optional-configuration)
  - [With Rustup](#with-rustup)
  - [Refreshing the GitHub GraphQL schema](#refreshing-the-github-graphql-schema)
  - [Developing](#developing)
  <!--toc:end-->

## Commits

**Please use [conventional commits](https://www.conventionalcommits.org/en/v1.0.0/).** PRs must also be titled in the same format.

This is so that [release-plz](https://release-plz.dev/) can properly categorize things in `CHANGELOG.md`.

## ast-grep

We use [ast-grep](https://ast-grep.github.io) for some custom linting rules. I highly recommend setting up the [editor integration](https://ast-grep.github.io/guide/tools/editors.html).

## With Nix (recommended)

**Requirements:**

- Nix (or [Lix](https://lix.systems/))
- [direnv](https://direnv.net/) (optional)
- [nix-direnv](https://github.com/nix-community/nix-direnv) (optional)

If you choose to use `direnv`, all you need to to is `direnv allow`. Otherwise, `nix develop` or your preferred way to activate `.#devShells.default`.

### Optional configuration

Optionally, you may choose to read from our Cachix cache.

URL: <https://jj-gh.cachix.org> \
Public Key: `jj-gh.cachix.org-1:N1uFBMDd9znlhDa68BRqLSXYzXXJ2+WHVuwxpGxCtDo=`

I also recommend setting the following repo configs for `jj`:

```toml
[aliases]
# .#dev is the same as `.#default` but with `doCheck = false`
# and skips completion installation, for faster local iteration
pr = ["util", "exec", "--", "nix", "run", ".#dev", "--", "pr"]

[fix.tools.treefmt]
command = ["treefmt", "--quiet", "--stdin", "$path"]
patterns = ["glob:'**/*'"]
```

## With Rustup

**Requirements:**

- [Rustup](https://rustup.rs/)

Run `rustup toolchain install` in the project to download and install the Rust toolchain.

## Refreshing the GitHub GraphQL schema

The GitHub GraphQL SDL is vendored at `src/gh/github.graphql`. To refresh it from
GitHub's public docs endpoint, run from the repo root:

```bash
nix run .#fetch-schema
# or, without nix:
python3 tools/fetch-schema.py
```

The script also strips description docstrings so the file stays under jj's 1MiB
snapshot limit. A weekly workflow does the same and opens a PR on drift.

## Developing

```bash
# if you use nix, you can use these to take advantage
# of nix caches and cachix
nix build             # build the CLI
nix run .# -- pr help # run the CLI
nix flake check       # run all checks
treefmt               # format all files

# if you use nix, these will be wrapped and use nix
# build caches
cargo nextest run           # run tests (or `cargo nt` alias)
cargo clippy --all-targets  # run clippy lints
cargo check --all-targets   # run checks
ast-grep scan --error       # enforce structural hygiene rules
ast-grep test               # test structural hygiene rules and diagnostics
cargo run -- pr help        # run the CLI

cargo run --quiet --package gen-manpage | man -l /dev/stdin # preview man page
```
