//! `flake.lock` model (schema version 7) with lossless, sorted-key round-trip.
//!
//! **Byte-compatible-writer invariant** (plan pillar): a parse → serialize
//! round-trip must reproduce what `nix flake update` writes, so a foreign lock
//! and a clinix-written lock are interchangeable. Two consequences drive this
//! model:
//!
//! 1. **Lossless.** Nothing is dropped, including fields clinix does not
//!    understand and source `type`s it does not support. That rules out a typed
//!    struct-per-source (named fields silently discard unknown keys), so
//!    `locked`/`original` are stored as ordered maps ([`Source`]) and typed
//!    access goes through accessors + [`LockedKind`].
//! 2. **Lexical key order.** Nix emits JSON with keys sorted lexically and
//!    2-space indent. `serde_json`'s default `Map` is `BTreeMap`-backed (sorted)
//!    and its pretty printer uses 2 spaces, so the struct field order below is
//!    declared alphabetically to match, and [`Source`] is a `BTreeMap`.
//!
//! The one formatting detail `serde_json` does not add is the trailing newline,
//! which [`FlakeLock::to_json`] appends.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;

/// A parsed `flake.lock`. Top-level keys are `nodes`, `root`, `version` —
/// already alphabetical, matching nix's output order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlakeLock {
    /// Every locked node, keyed by node name. `BTreeMap` gives nix's sorted key
    /// order on write.
    pub nodes: BTreeMap<String, Node>,
    /// The name of the root node (usually `"root"`).
    pub root: String,
    /// Lock schema version (7 for current nix).
    pub version: u32,
}

impl FlakeLock {
    /// Parse a `flake.lock` document.
    pub fn from_json(s: &str) -> Result<Self> {
        Ok(serde_json::from_str(s)?)
    }

    /// Serialize back to the on-disk form: 2-space indent, sorted keys, and the
    /// trailing newline nix writes. Byte-identical to `nix flake update`'s output
    /// for any lock this model round-trips (see the module invariant).
    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("FlakeLock is serializable");
        s.push('\n');
        s
    }

    /// The root node (`nodes[root]`), if present.
    pub fn root_node(&self) -> Option<&Node> {
        self.nodes.get(&self.root)
    }
}

/// One node in the lock graph. Fields are declared alphabetically (`flake`,
/// `inputs`, `locked`, `original`) so serialization matches nix's key order;
/// each is omitted when absent/empty, exactly as nix omits them (the `root` node
/// carries only `inputs`; a leaf carries `locked` + `original`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// Present (as `false`) only for non-flake inputs; nix omits it otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flake: Option<bool>,
    /// This node's edges to other nodes. Empty for leaves.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, InputRef>,
    /// The resolved, pinned source. Absent on the root node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<Source>,
    /// The unresolved source spec. Absent on the root node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original: Option<Source>,
}

/// An `inputs` edge value: either a **direct** reference to another node by name
/// (`"nixpkgs"`) or a **follows** path into the graph (`["vulnix", "nixpkgs"]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InputRef {
    /// A single node name.
    Direct(String),
    /// A follows path (walk these input names from the root).
    Follows(Vec<String>),
}

/// A `locked`/`original` source, kept as a lossless ordered map so no field is
/// dropped and keys serialize in nix's lexical order (module invariant). Typed
/// access is via the accessors and [`Source::kind`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Source(pub BTreeMap<String, Value>);

impl Source {
    fn get_str(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(Value::as_str)
    }

    /// The `type` discriminant (`"github"`, `"tarball"`, …), if present.
    pub fn source_type(&self) -> Option<&str> {
        self.get_str("type")
    }

    /// Classify the source by its `type` field.
    pub fn kind(&self) -> LockedKind {
        LockedKind::from_type(self.source_type())
    }

    /// The pinned revision (`rev`), unvalidated. Parse with
    /// [`crate::model::newtypes::Rev`] when a command needs a well-formed rev.
    pub fn rev(&self) -> Option<&str> {
        self.get_str("rev")
    }

    /// The content hash (`narHash`), unvalidated. Parse with
    /// [`crate::model::newtypes::NarHash`] at the boundary.
    pub fn nar_hash(&self) -> Option<&str> {
        self.get_str("narHash")
    }

    /// The fetch URL (`url`), for `tarball`/`git` sources.
    pub fn url(&self) -> Option<&str> {
        self.get_str("url")
    }

    /// The forge owner (`owner`), for `github`/`gitlab`/`sourcehut`.
    pub fn owner(&self) -> Option<&str> {
        self.get_str("owner")
    }

    /// The forge repo (`repo`).
    pub fn repo(&self) -> Option<&str> {
        self.get_str("repo")
    }

    /// The tracked branch/tag (`ref`).
    pub fn git_ref(&self) -> Option<&str> {
        self.get_str("ref")
    }

    fn from_pairs(pairs: &[(&str, &str)]) -> Source {
        Source(pairs.iter().map(|(k, v)| (k.to_string(), Value::from(*v))).collect())
    }

    /// A `github` **locked** source: `{narHash, owner, repo, rev, type}` — the
    /// shape `pin`/clinix write (deliberately no `lastModified`). Shared by
    /// `init` (build) and `update` (re-lock).
    pub fn github_locked(owner: &str, repo: &str, rev: &str, nar_hash: &str) -> Source {
        Source::from_pairs(&[
            ("narHash", nar_hash),
            ("owner", owner),
            ("repo", repo),
            ("rev", rev),
            ("type", "github"),
        ])
    }

    /// A `github` **original** tracking a branch/tag: `{owner, ref, repo, type}`.
    pub fn github_ref(owner: &str, repo: &str, git_ref: &str) -> Source {
        Source::from_pairs(&[
            ("owner", owner),
            ("ref", git_ref),
            ("repo", repo),
            ("type", "github"),
        ])
    }

    /// A `github` **original** frozen at a rev: `{owner, repo, rev, type}`.
    pub fn github_rev(owner: &str, repo: &str, rev: &str) -> Source {
        Source::from_pairs(&[
            ("owner", owner),
            ("repo", repo),
            ("rev", rev),
            ("type", "github"),
        ])
    }
}

/// The recognized source `type`s. `github` and `tarball` are the v0.1 pin paths
/// (plan decision 5); `git`/`path`/`indirect` parse but pinning support arrives
/// with the resolver. [`Other`](LockedKind::Other) keeps an unknown `type`
/// verbatim (lossless), and pin logic rejects it as `UnsupportedInput`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockedKind {
    Github,
    Tarball,
    Git,
    Path,
    Indirect,
    GitLab,
    SourceHut,
    Mercurial,
    /// A present-but-unrecognized `type`.
    Other(String),
    /// No `type` field (e.g. a node with no source).
    None,
}

impl LockedKind {
    fn from_type(t: Option<&str>) -> Self {
        match t {
            Some("github") => Self::Github,
            Some("tarball") => Self::Tarball,
            Some("git") => Self::Git,
            Some("path") => Self::Path,
            Some("indirect") => Self::Indirect,
            Some("gitlab") => Self::GitLab,
            Some("sourcehut") => Self::SourceHut,
            Some("mercurial") => Self::Mercurial,
            Some(other) => Self::Other(other.to_string()),
            std::option::Option::None => Self::None,
        }
    }
}
