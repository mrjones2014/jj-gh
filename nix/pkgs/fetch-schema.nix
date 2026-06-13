{ pkgs }:
pkgs.writeShellApplication {
  name = "fetch-schema";
  runtimeInputs = [ pkgs.python3 ];
  text = ''
    python3 ${../../tools/fetch-schema.py}
  '';
}
