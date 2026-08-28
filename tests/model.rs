//! Integration tests mirroring `src/model/`. Cargo treats a file directly under
//! `tests/` as a test crate; a crate root resolves `mod` files in its own
//! directory, so `#[path]` points at the `tests/model/` subtree that mirrors the
//! source layout. These exercise the public `clinix::model` API.

#[path = "model/lock.rs"]
mod lock;
#[path = "model/newtypes.rs"]
mod newtypes;
