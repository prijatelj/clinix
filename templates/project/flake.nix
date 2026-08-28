# flake.lock is written by BOTH `nix flake update` and `pin update`; they
# converge because neither holds state. Inputs below are hand-written; pin
# only ever touches this file via `pin freeze --write`, which swaps one URL.
#
# To freeze an input, replace its ref with a 40-hex rev (or run
# `pin freeze <name> --write`). Nix then has nothing to advance.
{
  description = "Project dev shell";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    # Sources that are not themselves flakes need `flake = false`.
    # The lock node shape is identical either way, so the reader does not care.
    # some-lib = {
    #   url = "github:owner/repo/v1.10.0";
    #   flake = false;
    # };
  };

  outputs =
    { self, nixpkgs, ... }:
    let
      sources = { inherit nixpkgs; };
      lib = nixpkgs.lib;
      systems = [ "x86_64-linux" "aarch64-linux" ];
    in
    {
      devShells = lib.genAttrs systems (system: {
        default = import ./shell.nix {
          inherit sources system;
          pkgs = import nixpkgs { inherit system; };
        };
      });
      legacyPackages = lib.genAttrs systems (system: import nixpkgs { inherit system; });
    };
}
