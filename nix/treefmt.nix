{ pkgs }:
pkgs.treefmt.withConfig {
  runtimeInputs = [
    pkgs.actionlint
    pkgs.nixfmt
    pkgs.prettier
    pkgs.rustfmt
    pkgs.tombi
    pkgs.yamlfmt
  ];
  settings = pkgs.lib.importTOML ../treefmt.toml;
}
