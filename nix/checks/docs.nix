{
  pkgs,
  commonArgs,
  update-docs,
}:
pkgs.runCommand "docs-check"
  {
    nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ pkgs.diffutils ];
  }
  ''
    mkdir -p $out
    cp ${../../DOCS.md} "$out/expected.md"
    touch "$out/flake.nix"
    cd "$out"
    ${update-docs}/bin/update-docs
    if ! diff -u expected.md DOCS.md; then
      echo "DOCS.md out of date; run \`nix run .#docs\`" >&2
      exit 1
    fi
  ''
