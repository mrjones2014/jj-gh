# Installation

## Requirements

- `jj` must be on your `PATH`.
- The repository must use the `jj` Git backend. The simple backend is not supported.
- `pr fetch` reads GitHub's special `refs/pull/<n>/head` refs via [gix](https://github.com/GitoxideLabs/gitoxide), because `jj` cannot yet fetch arbitrary refs.

## Nix

Add the flake input. The `stable` branch points to the latest release; use `github:mrjones2014/jj-gh` to follow `master`, or `github:mrjones2014/jj-gh/jj-gh-v0.3.1` to pin to a release tag instead:

```nix
{
  inputs.jj-gh.url = "github:mrjones2014/jj-gh/stable";
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

You can either use the overlay directly or use the `home-manager` module. The Nix package installs the manpage automatically; run `man jj-gh` to view it.

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
    # shell home-manager has enabled (fish/bash/nushell/zsh).
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

Optionally opt in to the Cachix binary cache:

- URL: <https://jj-gh.cachix.org>
- Public key: `jj-gh.cachix.org-1:N1uFBMDd9znlhDa68BRqLSXYzXXJ2+WHVuwxpGxCtDo=`

The `home-manager` module can install the `jj pr` alias and shell completion overlay for you via `programs.jujutsu.gh.aliases`. See [Shell completion](./completions.md) for manual setup with other install methods.

## Precompiled binary

Download and extract the archive for your platform from the [latest GitHub release](https://github.com/mrjones2014/jj-gh/releases/latest). The Linux and macOS archives contain the `jj-gh` binary and its manpage. For example, to install them under `~/.local`:

```sh
mkdir -p ~/.local/bin ~/.local/share/man/man1
install -m 755 jj-gh ~/.local/bin/jj-gh
install -m 644 jj-gh.1 ~/.local/share/man/man1/jj-gh.1
```

Ensure `~/.local/bin` is in your `PATH` and `~/.local/share/man` is in your `MANPATH`.

## With cargo-binstall

Requires [cargo-binstall](https://github.com/cargo-bins/cargo-binstall).

```sh
cargo binstall jj-gh
```

## From crates.io

Requires a Rust toolchain.

```sh
cargo install jj-gh
```

## From source

Requires a Rust toolchain. Clone this repository, then from the repo root:

```sh
cargo install --path .
```

## Set up a `jj` alias

Set up `pr` as a built-in `jj` subcommand so you can write `jj pr create <rev>`. If you use the `home-manager` module with an alias entry, this is already done for you.

```toml
# ~/.config/jj/config.toml
[aliases]
pr = ["util", "exec", "--", "jj-gh", "pr"]
```

After that, `jj pr create <rev>` and `jj pr fetch <pr-num>` work like any other `jj` subcommand. Continue with [Getting started](./getting-started.md).
