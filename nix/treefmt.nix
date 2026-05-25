{ rustToolchain }:
{
  projectRootFile = "flake.nix";
  programs = {
    rustfmt = {
      enable = true;
      package = rustToolchain;
    };
    prettier = {
      enable = true;
      includes = [
        "*.md"
        "*.gql"
      ];
      excludes = [ "CHANGELOG.md" ];
    };
    taplo.enable = true;
    yamlfmt.enable = true;
    nixfmt.enable = true;
    actionlint.enable = true;
  };
}
