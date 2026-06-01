self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  inherit (lib)
    mkEnableOption
    mkIf
    mkOption
    mkMerge
    optionalAttrs
    types
    ;

  cfg = config.programs.jujutsu.gh;

  # if Carapace is enabled, it eats the overlay; use `lib.mkOrder`
  # to make sure our completions come after Carapace.
  priority = 3000;

  jj_gh_table = lib.filterAttrs (_: v: v != null) cfg.settings;

  mkJjAliasArgv = subcmd: [
    "util"
    "exec"
    "--"
    "${cfg.package}/bin/jj-gh"
    subcmd
  ];

  mkOverlay =
    shell: aliasName: subcmd:
    pkgs.runCommand "jj-gh-${shell}-${aliasName}-overlay" { } ''
      ${cfg.package}/bin/jj-gh completions ${shell} \
        --jj-alias ${aliasName} \
        --subcommand ${subcmd} > $out
    '';
in
{
  options.programs.jujutsu.gh = {
    enable = mkEnableOption "jj-gh, opinionated jj tools for GitHub PR workflows";

    package = mkOption {
      type = types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "jj-gh.packages.\${system}.default";
      description = "The jj-gh package to install.";
    };

    aliases = mkOption {
      type = types.attrsOf types.str;
      default = {
        pr = "pr";
      };
      example = {
        pr = "pr";
      };
      description = ''
        Map of `jj` alias name -> `jj-gh` subcommand. Each entry:

        - Installs `programs.jujutsu.settings.aliases.<name>` dispatching to
          `jj-gh <subcommand>` (so e.g. `jj pr create` runs `jj-gh pr create`).
        - Drops a shell completion overlay for `jj <name> <tab>` into each
          enabled shell (fish/bash/zsh), so completions work out of the box.
      '';
    };

    settings = {
      gh_askpass = mkOption {
        type = types.nullOr (types.listOf types.str);
        default = null;
        example = [
          "op"
          "read"
          "op://Personal/github/token"
        ];
        description = "Askpass helper argv that prints a GitHub token on stdout.";
      };

      askpass_timeout_secs = mkOption {
        type = types.nullOr types.ints.unsigned;
        default = null;
        example = 20;
        description = "Timeout for the askpass helper.";
      };

      gh_token = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Plain GitHub token. Less safe than `gh_askpass` since the value ends
          up in the world-readable Nix store.
        '';
      };

      default_base_branch = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "main";
        description = "Fallback base branch when nothing smarter is detected.";
      };

      default_remote = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "origin";
        description = "Default git remote used for pushes and PR head lookup.";
      };

      upstream_remote = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "upstream";
        description = "Default git remote used for cross-fork PR target.";
      };

      pr_create_template_file = mkOption {
        type = types.nullOr types.path;
        default = null;
        description = "Path to a markdown file used as the PR body template.";
      };

      pr_create_template = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = ''description ++ "\n"'';
        description = ''
          jj template string used to render the PR body. Evaluated against the
          revset being PR'd in chronological order, with `pr_title`, `pr_base`,
          and `pr_head_branch` aliases injected.
        '';
      };

      pr_log_template = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = ''description ++ " #" ++ pr_number ++ "\n"'';
        description = ''
          jj template string used to render the `pr log` subcommand. Template aliases
          for PR metadata are injected. See https://github.com/mrjones2014/jj-gh#template-aliases
        '';
      };

      pr_restack_template = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = ''description ++ " #" ++ pr_number ++ "\n"'';
        description = ''
          jj template string used to render the `pr restack` subcommand.
          By default, uses `pr_log_template`. Template aliases
          for PR metadata are injected. See https://github.com/mrjones2014/jj-gh#template-aliases
        '';
      };

      draft = mkOption {
        type = types.nullOr types.bool;
        default = null;
        description = "Open PRs as drafts by default.";
      };

      auto_merge = mkOption {
        type = types.nullOr types.bool;
        default = null;
        description = "Enable auto-merge on PRs after creation by default.";
      };

      auto_merge_method = mkOption {
        type = types.nullOr (
          types.enum [
            "merge"
            "squash"
            "rebase"
          ]
        );
        default = null;
        example = "squash";
        description = "Default GitHub merge method to use when auto-merge is enabled.";
      };

      editor = mkOption {
        type = types.nullOr (types.listOf types.str);
        default = null;
        example = [
          "nvim"
          "+10"
        ];
        description = "Editor argv used for the PR frontmatter buffer.";
      };

      pr_fetch_bookmark_template = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = ''"pr-" ++ pr_number ++ "/" ++ pr_branch'';
        description = ''
          jj template string used to render the local bookmark name created by
          `jj pr fetch`. Evaluated against `root()` with `pr_*` aliases
          pre-populated from the PR's metadata.
        '';
      };

      nerdfonts = mkOption {
        type = types.nullOr types.bool;
        default = null;
        description = "Use nerdfont icons in `jj pr log` output.";
      };
    };
  };

  config = mkIf (cfg.enable && config.programs.jujutsu.enable) (mkMerge [
    {
      home.packages = [ cfg.package ];
      programs.jujutsu.settings = mkMerge [
        {
          aliases = lib.mapAttrs (_: subcmd: lib.mkDefault (mkJjAliasArgv subcmd)) cfg.aliases;
        }
        (optionalAttrs (jj_gh_table != { }) { "jj-gh" = jj_gh_table; })
      ];
    }

    # fish: source overlays from interactiveShellInit. Fish autoloads
    # completion files by filename match (`<cmd>.fish`), so dropping
    # `jj-gh-<alias>-overlay.fish` into `completions/` would never fire on
    # `jj <tab>`. Sourcing from interactive init registers the rules
    # against `jj` directly; fish then unions them with jj's own.
    (mkIf (config.programs.fish.enable && cfg.aliases != { }) {
      programs.fish.interactiveShellInit = lib.mkOrder priority (
        lib.concatMapStringsSep "\n" (n: ''
          source ${mkOverlay "fish" n cfg.aliases.${n}}
        '') (lib.attrNames cfg.aliases)
      );
    })

    # bash: source overlays from initExtra. The overlays self-bootstrap —
    # each one calls bash-completion's dynamic loader to force jj's own
    # completion to load before snapshotting the prior `complete -F`
    # handler, so we don't need to `eval "$(jj util completion bash)"`
    # here.
    (mkIf (config.programs.bash.enable && cfg.aliases != { }) {
      programs.bash.initExtra = lib.mkOrder priority (
        lib.concatMapStringsSep "\n" (n: ''
          source ${mkOverlay "bash" n cfg.aliases.${n}}
        '') (lib.attrNames cfg.aliases)
      );
    })

    # zsh: source overlays from initExtra. initExtra runs after compinit
    # in home-manager's .zshrc, so `_comps[jj]` is already populated from
    # `_jj` in fpath (nixpkgs jujutsu ships it under
    # share/zsh/site-functions) and the overlays can snapshot it directly.
    (mkIf (config.programs.zsh.enable && cfg.aliases != { }) {
      programs.zsh.initExtra = lib.mkOrder priority (
        lib.concatMapStringsSep "\n" (n: ''
          source ${mkOverlay "zsh" n cfg.aliases.${n}}
        '') (lib.attrNames cfg.aliases)
      );
    })
  ]);
}
