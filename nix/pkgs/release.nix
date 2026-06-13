{
  pkgs,
  rustToolchain,
  release-plz-patched,
}:
pkgs.writeShellApplication {
  name = "release";
  runtimeInputs = [
    rustToolchain
    release-plz-patched
    pkgs.cargo-semver-checks
    pkgs.git
  ];
  text = ''
    release-plz release-pr --git-token "$GITHUB_TOKEN" "$@"
    release-plz release --git-token "$GITHUB_TOKEN" "$@"
  '';
}
