//! `clinix config show`: the **effective** configuration as valid TOML — every
//! setting, including the defaults no file states — with its provenance as
//! minimal comments.
//!
//! Within each section, values are grouped by where they came from, highest
//! precedence first: one `# <file>` comment per config file in the `use` chain,
//! then `# discovered` (locks found in `locks_dir`), then `# defaults`. So what
//! you set reads first, and everything under `# defaults` is clinix's own.
//!
//! Values are printed **as written** (paths unresolved), and `use` is omitted:
//! the imports are already folded in, so printing it would apply them twice if
//! the output were reused as a config.

use std::path::Path;

use crate::env::config::{
	Config, DEFAULT_KEEP_N_PRIOR, DEFAULT_LOCKS_DIR, DEFAULT_REGISTRY, DEFAULT_SEEDS_DIR,
	DEFAULT_SHORTHAND, Layer, Settings, load_layers,
};
use crate::error::Result;

/// How one setting merges across layers, and its default.
enum Kind {
	/// The highest layer that sets it wins; `None` default = unset.
	Scalar(Option<String>),
	/// Concatenated across layers, highest first.
	List,
	/// Merged per key, the highest layer winning each key.
	Map,
}

/// One config section: its path (`["env", "gc"]`) and its settings in order.
type Section = (&'static [&'static str], Vec<(&'static str, Kind)>);

/// The settings `config show` reports, per section, in output order. Defaults
/// come from the same constants the settings code reads.
fn schema() -> Vec<Section> {
	let quoted = |s: &str| toml::Value::String(s.to_string()).to_string();
	let keep = || Kind::Scalar(Some(DEFAULT_KEEP_N_PRIOR.to_string()));
	vec![
		(
			&["clinix"],
			vec![(
				"shorthand",
				Kind::Scalar(Some(DEFAULT_SHORTHAND.to_string())),
			)],
		),
		(
			&["env"],
			vec![
				("registry", Kind::Scalar(Some(quoted(DEFAULT_REGISTRY)))),
				("locks_dir", Kind::Scalar(Some(quoted(DEFAULT_LOCKS_DIR)))),
				("nixpkgs_config", Kind::Scalar(None)),
			],
		),
		(&["env", "locks"], vec![("", Kind::Map)]),
		(
			&["env", "gc"],
			vec![
				("keep_n_prior_roots", keep()),
				("keep_n_prior_project_roots", keep()),
				("keep_n_prior_stack_roots", keep()),
			],
		),
		(
			&["env", "seeds"],
			vec![
				("dir", Kind::Scalar(Some(quoted(DEFAULT_SEEDS_DIR)))),
				("sources", Kind::List),
				("ignore", Kind::List),
				("namespace", Kind::Scalar(None)),
			],
		),
	]
}

/// Render the effective config for `cfg` (see the module docs).
pub fn show(cfg: &Config) -> Result<String> {
	let layers = load_layers(&cfg.config_dir)?;
	let settings = Settings::fold(&layers);
	let discovered: Vec<String> = crate::env::all_locks(cfg, &settings)
		.into_iter()
		.filter(|(_, (_, origin))| *origin == crate::env::LockOrigin::Discovered)
		.map(|(name, _)| name)
		.collect();
	let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
	Ok(render(&layers, &discovered, home.as_deref()))
}

/// Pure rendering over the loaded layers (highest precedence first) and the
/// discovered lock names.
fn render(layers: &[Layer], discovered: &[String], home: Option<&Path>) -> String {
	let labels: Vec<String> = layers.iter().map(|l| tilde(&l.path, home)).collect();
	let mut out = String::new();
	for (section, keys) in schema() {
		// One line group per layer, then `discovered`, then `defaults`.
		let mut groups: Vec<Vec<String>> = vec![Vec::new(); layers.len()];
		let mut found = Vec::new();
		let mut defaults = Vec::new();
		let tables: Vec<Option<&toml::Table>> = layers
			.iter()
			.map(|l| section_of(&l.table, section))
			.collect();

		for (key, kind) in keys {
			match kind {
				Kind::Scalar(default) => {
					match tables
						.iter()
						.position(|t| t.and_then(|t| t.get(key)).is_some())
					{
						Some(i) => groups[i].push(format!(
							"{} = {}",
							toml_key(key),
							inline(&tables[i].unwrap()[key])
						)),
						None => defaults.push(match default {
							Some(v) => format!("{} = {v}", toml_key(key)),
							None => format!("# {} — unset", toml_key(key)),
						}),
					}
				}
				Kind::List => {
					let parts: Vec<(usize, &Vec<toml::Value>)> = tables
						.iter()
						.enumerate()
						.filter_map(|(i, t)| Some((i, (*t)?.get(key)?.as_array()?)))
						.collect();
					match parts.first() {
						None => defaults.push(format!("{} = []", toml_key(key))),
						Some(&(top, _)) => {
							groups[top].push(list(key, &parts, &labels));
						}
					}
				}
				Kind::Map => {
					let mut placed = std::collections::BTreeSet::new();
					for (i, table) in tables.iter().enumerate() {
						for (name, value) in table.iter().flat_map(|t| t.iter()) {
							if placed.insert(name.clone()) {
								groups[i].push(format!("{} = {}", toml_key(name), inline(value)));
							}
						}
					}
					let dir = locks_dir_as_written(layers);
					for name in discovered.iter().filter(|n| !placed.contains(*n)) {
						let path = format!("{}/{name}.lock", dir.trim_end_matches('/'));
						found.push(format!(
							"{} = {}",
							toml_key(name),
							toml::Value::String(path)
						));
					}
				}
			}
		}

		out.push_str(&format!("[{}]\n", section.join(".")));
		for (label, lines) in labels.iter().zip(&groups) {
			push_group(&mut out, label, lines);
		}
		push_group(&mut out, "discovered", &found);
		push_group(&mut out, "defaults", &defaults);
		out.push('\n');
	}
	out.pop(); // no trailing blank line
	out
}

fn push_group(out: &mut String, label: &str, lines: &[String]) {
	if lines.is_empty() {
		return;
	}
	out.push_str(&format!("# {label}\n"));
	for line in lines {
		out.push_str(line);
		out.push('\n');
	}
}

/// A list merged from one or more layers, printed under the group of its highest
/// layer (`parts[0]`). Items from any *other* layer are headed by that layer's
/// comment, so a list set in one file carries no inner comments at all.
fn list(key: &str, parts: &[(usize, &Vec<toml::Value>)], labels: &[String]) -> String {
	let nonempty: Vec<_> = parts
		.iter()
		.filter(|(_, items)| !items.is_empty())
		.collect();
	if nonempty.is_empty() {
		return format!("{} = []", toml_key(key));
	}
	let group = parts[0].0;
	let mut s = format!("{} = [\n", toml_key(key));
	for (i, items) in &nonempty {
		if *i != group {
			s.push_str(&format!("  # {}\n", labels[*i]));
		}
		for item in items.iter() {
			s.push_str(&format!("  {},\n", inline(item)));
		}
	}
	s.push(']');
	s
}

/// `[env].locks_dir` as written by the highest layer that sets it, else the
/// default — the prefix a discovered lock is shown under.
fn locks_dir_as_written(layers: &[Layer]) -> String {
	layers
		.iter()
		.find_map(|l| section_of(&l.table, &["env"])?.get("locks_dir")?.as_str())
		.unwrap_or(DEFAULT_LOCKS_DIR)
		.to_string()
}

/// The table at `path` inside `table`, if every step is a table.
fn section_of<'a>(table: &'a toml::Table, path: &[&str]) -> Option<&'a toml::Table> {
	path.iter()
		.try_fold(table, |t, step| t.get(*step)?.as_table())
}

/// A TOML key: bare when it can be, else a quoted string.
fn toml_key(key: &str) -> String {
	if !key.is_empty()
		&& key
			.chars()
			.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
	{
		key.to_string()
	} else {
		toml::Value::String(key.to_string()).to_string()
	}
}

/// A value on one line: arrays and tables inline, everything else as TOML.
fn inline(value: &toml::Value) -> String {
	match value {
		toml::Value::Array(items) => {
			let items: Vec<String> = items.iter().map(inline).collect();
			format!("[{}]", items.join(", "))
		}
		toml::Value::Table(table) => {
			let entries: Vec<String> = table
				.iter()
				.map(|(k, v)| format!("{} = {}", toml_key(k), inline(v)))
				.collect();
			format!("{{ {} }}", entries.join(", "))
		}
		other => other.to_string(),
	}
}

/// A path for a provenance comment, with `$HOME` shown as `~`.
fn tilde(path: &Path, home: Option<&Path>) -> String {
	match home.and_then(|h| path.strip_prefix(h).ok()) {
		Some(rest) => format!("~/{}", rest.display()),
		None => path.display().to_string(),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::path::PathBuf;

	fn layer(path: &str, body: &str) -> Layer {
		Layer {
			path: PathBuf::from(path),
			table: toml::from_str(body).unwrap(),
			settings: toml::from_str(body).unwrap(),
		}
	}

	#[test]
	fn groups_each_section_by_source_then_discovered_then_defaults() {
		let layers = [
			layer(
				"/h/.config/clinix/config.toml",
				"[env.locks]\ndefault = \"~/nixos/flake.lock\"\n[env.seeds]\nsources = [\"/top\"]\n",
			),
			layer(
				"/h/base.toml",
				"[env]\nlocks_dir = \"my-locks\"\n[env.seeds]\nsources = [\"/base\"]\n",
			),
		];
		let out = render(&layers, &["unstable".to_string()], Some(Path::new("/h")));
		// Set values first under their file (`$HOME` as `~`), defaults last.
		assert!(out.contains(
			"[env]\n# ~/base.toml\nlocks_dir = \"my-locks\"\n# defaults\nregistry = \"envs\"\n\
			 # nixpkgs_config — unset\n"
		));
		// A discovered lock is shown under the locks_dir as written.
		assert!(out.contains(
			"[env.locks]\n# ~/.config/clinix/config.toml\ndefault = \"~/nixos/flake.lock\"\n\
			 # discovered\nunstable = \"my-locks/unstable.lock\"\n"
		));
		// A list from two files: under the highest, with the other file's items labelled.
		assert!(out.contains(
			"# ~/.config/clinix/config.toml\nsources = [\n  \"/top\",\n  # ~/base.toml\n  \"/base\",\n]"
		));
		// An untouched section is all defaults.
		assert!(out.contains("[clinix]\n# defaults\nshorthand = true\n"));
		// The output is a valid config: it parses back as settings.
		let _: Settings = toml::from_str(&out).expect("valid config TOML");
	}

	#[test]
	fn with_no_config_everything_is_a_default() {
		let out = render(&[], &[], None);
		assert!(!out.contains("# /"), "no file provenance");
		assert!(out.contains("[env.gc]\n# defaults\nkeep_n_prior_roots = 0\n"));
		assert!(out.contains("sources = []"));
		assert!(out.contains("[env.seeds]\n# defaults\ndir = \"seeds\"\n"));
		let _: Settings = toml::from_str(&out).expect("valid config TOML");
	}
}
