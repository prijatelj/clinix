//! A minimal step-progress reporter for long-running commands.
//!
//! Progress is a **declared side effect**: the lines go to **stderr**, so a
//! command's stdout stays reserved for its primary, pipeable output (the final
//! result summary). Reporting is on by default; a `--quiet`/`-q` command builds a
//! [`Progress::silent`], which makes every method a no-op — the counter is shared
//! across helpers (e.g. `init` and the `build_nixpkgs_lock` it calls) so numbering
//! stays continuous.

use std::cell::Cell;

/// Reports discrete, numbered steps to stderr as `clinix: [k/n] <msg>`. Built with
/// the total step count `n`; [`Progress::silent`] suppresses all output.
///
/// `step` takes `&self` (interior `Cell` counter) so one reporter can be threaded
/// by shared reference through the call chain that performs the steps.
pub struct Progress {
	total: usize,
	done: Cell<usize>,
	enabled: bool,
}

impl Progress {
	/// A reporter that will print `total` numbered steps to stderr.
	pub fn new(total: usize) -> Self {
		Self {
			total,
			done: Cell::new(0),
			enabled: true,
		}
	}

	/// A no-op reporter — every method does nothing (`--quiet`, or a caller that
	/// wants the shared helper without its progress, e.g. seed lazy-locking).
	pub fn silent() -> Self {
		Self {
			total: 0,
			done: Cell::new(0),
			enabled: false,
		}
	}

	/// Announce the next step (`clinix: [k/n] <msg>`) **before** doing it, so the
	/// user sees what is running. Increments the shared counter.
	pub fn step(&self, msg: &str) {
		if !self.enabled {
			return;
		}
		let k = self.done.get() + 1;
		self.done.set(k);
		eprintln!("clinix: [{k}/{}] {msg}", self.total);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn silent_never_advances_and_new_counts() {
		let s = Progress::silent();
		s.step("a");
		s.step("b");
		assert_eq!(s.done.get(), 0, "silent is a no-op");

		let p = Progress::new(2);
		p.step("a");
		p.step("b");
		assert_eq!(p.done.get(), 2, "shared counter advances");
	}
}
