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

  cfg = config.programs.jj.gh;

  jjGhTable = lib.filterAttrs (_: v: v != null) {
    inherit (cfg.settings)
      gh_askpass
      askpass_timeout_secs
      gh_token
      default_base_branch
      template_path
      draft
      editor
      pr_fetch_bookmark_template
      ;
  };
in
{
  options.programs.jj.gh = {
    enable = mkEnableOption "jj-gh, opinionated jj tools for GitHub PR workflows";

    package = mkOption {
      type = types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "jj-gh.packages.\${system}.default";
      description = "The jj-gh package to install.";
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

      template_path = mkOption {
        type = types.nullOr types.path;
        default = null;
        description = "PR template path.";
      };

      draft = mkOption {
        type = types.nullOr types.bool;
        default = null;
        description = "Open PRs as drafts by default.";
      };

      editor = mkOption {
        type = types.nullOr (types.listOf types.str);
        default = null;
        example = [
          "nvim"
          "+8"
        ];
        description = "Editor argv used for the PR frontmatter buffer.";
      };

      pr_fetch_bookmark_template = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "pr-{number}/{branch}";
        description = "Bookmark name template for `jj pr fetch`.";
      };
    };
  };

  config = (mkIf cfg.enable && config.programs.jujutsu.enable) {
    home.packages = [ cfg.package ];
    programs.jujutsu.settings = mkMerge [
      {
        aliases.pr = lib.mkDefault [
          "util"
          "exec"
          "--"
          "${cfg.package}/bin/jj-gh"
          "pr"
        ];
      }
      (optionalAttrs (jjGhTable != { }) { "jj-gh" = jjGhTable; })
    ];
  };
}
