# clinix development shell — the toolchain to build/test clinix, plus the external
# binaries clinix shells out to at runtime (declared once here; mirrored for
# discoverability in Cargo.toml's [package.metadata.external-tools]).
#
# Uses the ambient <nixpkgs> for convenience (matches `nix-shell -p cargo rustc`).
# clinix advocates pinning; pin this with a committed flake.lock when it matters.
#
#   nix-shell                          # required tools only
#   nix-shell --arg withSkopeo true    # + daemon-free --from digest fallback
#   nix-shell --arg withDocker true    # + docker (export docker --load, --from fallback)
#   nix-shell --run 'cargo test'
#
# The optional flags are Nix's analog of `cargo --features`: shell.nix is a
# function, so its optional deps are gated by boolean args (default off) via
# `lib.optional`, opted in with `--arg`. There is no global feature registry.
{
  pkgs ? import <nixpkgs> { },
  # Optional fallbacks for authenticated `--from` image pins (off by default —
  # the required set uses `curl` for public images).
  withSkopeo ? false, # daemon-free digest resolution
  withDocker ? false, # daemon-based digest resolution + `export docker --load`
}:

let
  inherit (pkgs) lib;
in
pkgs.mkShell {
  name = "clinix-dev";

  # Rust toolchain (edition 2024 — needs a recent rustc/cargo).
  nativeBuildInputs = with pkgs; [
    cargo
    rustc
    clippy
    rustfmt
    rust-analyzer
  ];

  # Required runtime shell-outs — clinix commands error clearly if absent. The
  # optional fallbacks are appended only when their `--arg` is set.
  buildInputs =
    (with pkgs; [
      git
      nix # provides nix-instantiate / nix-store / nix-build / nix-shell / nix-hash
      nix-prefetch-scripts # nix-prefetch-url
      curl # OCI registry digest resolution (export docker --from)
    ])
    ++ lib.optional withSkopeo pkgs.skopeo
    ++ lib.optional withDocker pkgs.docker;

  shellHook = ''
    echo "clinix-dev: $(cargo --version 2>/dev/null || echo 'cargo missing')"
  '';
}
