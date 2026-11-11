# clinix development shell — the toolchain to build/test clinix, plus the external
# binaries clinix shells out to at runtime (declared once here; mirrored for
# discoverability in Cargo.toml's [package.metadata.external-tools]).
{
  sources ? (
    let
      lock = builtins.fromJSON (builtins.readFile ./flake.lock);
      fetch =
        node:
        let
          i = lock.nodes.${node}.locked;
        in
        if i.type == "github" then
          builtins.fetchTarball {
            url = "https://github.com/${i.owner}/${i.repo}/archive/${i.rev}.tar.gz";
            sha256 = i.narHash;
          }
        else if i.type == "git" then
          (builtins.fetchGit { inherit (i) url rev; }).outPath
        else
          throw "shell.nix: unsupported input type '${i.type}' on node '${node}'";
    in
    assert lock.version == 7;
    builtins.mapAttrs (_: fetch) lock.nodes.root.inputs
  ),

  system ? builtins.currentSystem,

  pkgs ? import sources.nixpkgs { inherit system; },
}:
pkgs.mkShell {
  name = "clinix-dev";
  packages = with pkgs; [
    cargo
    rustc
    clippy
    rustfmt
    rust-analyzer
    git
    nix-prefetch-scripts # nix-prefetch-url
    curl # OCI registry digest resolution (export docker --from)
  ];

  shellHook = ''
    echo "clinix-dev: $(cargo --version 2>/dev/null || echo 'cargo missing')"
  '';
}
