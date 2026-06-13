{
  pkgs,
  gen-docs,
  treefmtEval,
}:
pkgs.writeShellApplication {
  name = "update-docs";
  runtimeInputs = [
    gen-docs
    treefmtEval.config.build.wrapper
  ];
  text = ''
    gen-docs > DOCS.md
    treefmt --no-cache DOCS.md
  '';
}
