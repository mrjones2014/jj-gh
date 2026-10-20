{
  pkgs,
  gen-docs,
  treefmt,
}:
pkgs.writeShellApplication {
  name = "update-docs";
  runtimeInputs = [
    gen-docs
    treefmt
  ];
  text = ''
    gen-docs > DOCS.md
    treefmt --no-cache DOCS.md
  '';
}
