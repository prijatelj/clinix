# clinix dev tasks. Cargo runs inside a vanilla nix-shell (not the dev_env
# `shell rust`, which clinix is replacing). See notes/clinix/design/testing.md.

_rust := "nix-shell -p rust-analyzer clippy rustfmt cargo rustc --run"

# Fast suite: unit + model integration + nix-free CLI tests (L1–L3).
test:
	{{_rust}} 'cargo test'

# Nix end-to-end tests (L4): needs nix + network.
test-e2e:
	{{_rust}} 'cargo test -- --ignored'

# Everything, including the ignored nix E2E.
test-all:
	{{_rust}} 'cargo test -- --include-ignored'

fmt:
	{{_rust}} 'cargo fmt'

build:
	{{_rust}} 'cargo build'
