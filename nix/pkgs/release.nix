{ pkgs }:
pkgs.writeShellApplication {
  name = "release";
  runtimeInputs = [
    pkgs.cargo
    pkgs.rustc
    # `cargo publish` builds the crate outside of the Nix store, and nixpkgs'
    # rustc links with the Nix C compiler, which can't find the system
    # libraries. So provide the C compiler and OpenSSL from nixpkgs too.
    pkgs.stdenv.cc
    pkgs.pkg-config
    pkgs.release-plz
    pkgs.cargo-semver-checks
    pkgs.git
  ];
  runtimeEnv.PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig";
  text = ''
    release-plz release-pr --git-token "$GITHUB_TOKEN" "$@"
    release-plz release --git-token "$GITHUB_TOKEN" "$@"
  '';
}
