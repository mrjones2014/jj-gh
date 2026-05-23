{ rustToolchain }:
{
  projectRootFile = "flake.nix";
  programs = {
    rustfmt = {
      enable = true;
      package = rustToolchain;
    };
    taplo.enable = true;
    yamlfmt.enable = true;
    nixfmt.enable = true;
    actionlint.enable = true;
  };
}
