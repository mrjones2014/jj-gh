{ pkgs }:
# TODO remove when nixpkgs has version 0.3.170
pkgs.release-plz.overrideAttrs (old: rec {
  version = "0.3.170";
  src = old.src.override {
    hash = "sha256-iOwpEYNlBeu3Xr+X02rHsw0fr5lfO7v1h+Zc1UG7W6A=";
  };
  cargoDeps = pkgs.rustPlatform.fetchCargoVendor {
    inherit src;
    name = "release-plz-${version}-vendor";
    hash = "sha256-7bxLpn37uyy8xFT5aRgN9321kbJ1ZFFo0kc1aLGxH1k=";
  };
  patches = [ ];
})
