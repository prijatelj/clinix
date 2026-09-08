# clinix dev tasks. Cargo runs inside a vanilla nix-shell (not the dev_env
# `shell rust`, which clinix is replacing). See notes/clinix/design/testing.md.

_rust := "nix-shell -p rust-analyzer clippy rustfmt cargo rustc --run"

# Fast suite: unit + model integration + nix-free CLI tests (L1–L3).
test:
	{{_rust}} 'cargo test'

# Nix end-to-end tests (L4): needs nix + network. Run SERIALLY
# (`--test-threads=1`): these each shell out to git/nix-prefetch-url and nix
# builds, so running them concurrently contends on the network/nix daemon and
# yields spurious failures. Serial is the supported way to run the e2e.
test-e2e:
	{{_rust}} 'cargo test --test e2e -- --ignored --test-threads=1'

# Everything, including the ignored nix E2E (serial — see test-e2e).
test-all:
	{{_rust}} 'cargo test -- --include-ignored --test-threads=1'

fmt:
	{{_rust}} 'cargo fmt'

build:
	{{_rust}} 'cargo build'
