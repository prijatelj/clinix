//! First-party **extensions** (ADR-6 seam): interop targets that import only the
//! env read + external tools, never the `flake.lock`/Nix-lock internals. Currently
//! the docker image export ([`export_docker`]) and its OCI registry digest
//! resolver ([`oci`]). See `notes/clinix/design/export.md`.

pub mod export_docker;
pub mod oci;
