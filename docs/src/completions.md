# Shell completions

`jj-gh completions <shell>` prints a completion script for the `jj-gh` binary. jj's own completion script does not know about user-defined aliases, so when you invoke jj-gh through an alias such as `jj pr`, also emit a small overlay:

```sh
jj-gh completions <shell> --jj-alias pr --subcommand pr
```

`--jj-alias` and `--subcommand` must be passed together. Currently `pr` is the only supported subcommand tree. The overlay registers completions for `jj pr <tab>` on top of jj's script; source it **after** `jj util completion <shell>` so it can delegate to jj's completer when the alias is not being completed. Overlays for multiple aliases chain safely.

Standalone scripts support `bash`, `elvish`, `fish`, `nushell`, `powershell`, and `zsh`. The alias overlay supports `bash`, `fish`, `nushell`, and `zsh`.

## Manual setup

### fish

```sh
jj util completion fish | source
jj-gh completions fish --jj-alias pr --subcommand pr | source
```

### bash

```sh
eval "$(jj util completion bash)"
eval "$(jj-gh completions bash --jj-alias pr --subcommand pr)"
```

### zsh (after `compinit`)

```sh
source <(jj util completion zsh)
source <(jj-gh completions zsh --jj-alias pr --subcommand pr)
```

### nushell

```sh
jj util completion nushell | save -f completions-jj.nu
jj-gh completions nushell --jj-alias pr --subcommand pr | save -f completions-jj-gh-pr.nu
source completions-jj.nu
source completions-jj-gh-pr.nu
```

## Home-manager automation

If you install with the flake's home-manager module, this is automatic. Enable the shells you use and list your aliases; each entry installs the jj alias and drops the matching completion overlay into every enabled shell:

```nix
{ jj-gh, ... }:
{
  imports = [ jj-gh.homeManagerModules.default ];

  programs.fish.enable = true;
  programs.bash.enable = true;
  programs.nushell.enable = true;
  programs.zsh.enable = true;

  programs.jujutsu.gh = {
    enable = true;
    aliases = { pr = "pr"; };
    settings = {
      gh_askpass = [ "op" "read" "op://Private/GitHub/token" ];
    };
  };
}
```

Manual setup is only needed for `cargo install` / from-source installations, or for shells without module support (`elvish`, `powershell` standalone scripts only).
