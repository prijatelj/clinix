//! OCI/Docker registry **digest resolution** — the narrow "tag → manifest digest"
//! slice of the Distribution API v2, orchestrated over `curl` (no pure-Rust TLS
//! dep; consistent with clinix's shell-out-for-network model). Falls back to
//! `skopeo`/`docker` for authenticated images. See `notes/clinix/design/export.md` §4.
//!
//! The public entry is [`pin_reference`]: given `nvidia/cuda:12.4.1` it returns
//! `nvidia/cuda:12.4.1@sha256:…`, so a Dockerfile `FROM` is reproducible forever.

use std::collections::HashMap;
use std::process::Command;

use crate::error::{ClinixError, Result};

const DOCKER_HUB: &str = "registry-1.docker.io";
/// The four manifest media types a registry may answer a tag with (single image
/// or multi-arch index, docker or OCI). Sent as one comma-joined `Accept`.
const ACCEPT: &str = "application/vnd.docker.distribution.manifest.v2+json, \
	application/vnd.docker.distribution.manifest.list.v2+json, \
	application/vnd.oci.image.manifest.v1+json, \
	application/vnd.oci.image.index.v1+json";

/// A parsed image reference split into what the API needs and what to re-emit.
#[derive(Debug, PartialEq, Eq)]
pub struct ImageRef {
	/// Registry host used for the API call (`registry-1.docker.io` for Docker Hub).
	pub registry: String,
	/// API repository path (`library/ubuntu` for a bare Docker Hub `ubuntu`).
	pub repo: String,
	/// The tag (`latest` when none was given).
	pub tag: String,
	/// An already-pinned digest (`sha256:…`), if the reference carried `@`.
	pub digest: Option<String>,
	/// The user's name as typed (registry/repo, minus tag/digest) — re-emitted so
	/// the pin keeps their short form (`ubuntu:latest@sha256:…`, not the API repo).
	pub display: String,
}

/// Parse an image reference per docker's rules: split off `@digest`, then a
/// leading registry (a first path component containing `.`/`:` or `localhost`),
/// then the tag (last `:` in the remainder). Docker Hub gets the `library/` prefix
/// for single-name repos. Pure — unit-tested.
pub fn parse_ref(image: &str) -> ImageRef {
	// 1. digest.
	let (name_tag, digest) = match image.split_once('@') {
		Some((n, d)) => (n, Some(d.to_string())),
		None => (image, None),
	};
	// 2. registry vs the rest.
	let (registry, remainder, had_registry) = match name_tag.split_once('/') {
		Some((first, rest))
			if first.contains('.') || first.contains(':') || first == "localhost" =>
		{
			(first.to_string(), rest.to_string(), true)
		}
		_ => (DOCKER_HUB.to_string(), name_tag.to_string(), false),
	};
	// 3. tag (last `:`, but not if the suffix looks like a path).
	let (repo_part, tag) = match remainder.rsplit_once(':') {
		Some((r, t)) if !t.contains('/') => (r.to_string(), t.to_string()),
		_ => (remainder.clone(), "latest".to_string()),
	};
	// The display name is the user's registry/repo (minus tag/digest).
	let display = if had_registry {
		format!("{registry}/{repo_part}")
	} else {
		repo_part.clone()
	};
	// 4. Docker Hub library prefix for single-name repos.
	let repo = if !had_registry && !repo_part.contains('/') {
		format!("library/{repo_part}")
	} else {
		repo_part
	};
	ImageRef {
		registry,
		repo,
		tag,
		digest,
		display,
	}
}

/// Resolve `image` to a fully **digest-pinned** reference (`display:tag@sha256:…`).
/// An already-`@sha256`-pinned reference is returned unchanged. `use_docker_pull`
/// forces the heavy `docker pull` + inspect fallback (`--pull`).
pub fn pin_reference(image: &str, use_docker_pull: bool) -> Result<String> {
	let r = parse_ref(image);
	if r.digest.is_some() {
		// Already pinned — keep the user's exact reference verbatim.
		return Ok(image.to_string());
	}
	let digest = resolve_digest(&r, use_docker_pull)?;
	Ok(format!("{}:{}@{}", r.display, r.tag, digest))
}

/// Resolve just the `sha256:…` digest, trying the anonymous curl flow first, then
/// `skopeo`/`docker` for authenticated images.
fn resolve_digest(r: &ImageRef, use_docker_pull: bool) -> Result<String> {
	match resolve_via_curl(r) {
		Ok(d) => return Ok(d),
		Err(ResolveErr::Auth) => { /* fall through to tool fallbacks */ }
		Err(ResolveErr::Other(e)) => return Err(e),
	}
	let full = format!("{}:{}", r.display, r.tag);
	if tool_present("skopeo") {
		if let Some(d) = resolve_via_skopeo(&full) {
			return Ok(d);
		}
	}
	if use_docker_pull && tool_present("docker") {
		if let Some(d) = resolve_via_docker_pull(&full) {
			return Ok(d);
		}
	}
	Err(ClinixError::Config(format!(
		"could not resolve a digest for `{full}` — it likely needs registry \
		 authentication. Install `skopeo` (or pass `--pull` with docker logged in), \
		 or give a pre-pinned `{}@sha256:…` directly.",
		r.display
	)))
}

/// Curl-flow errors: an auth wall (→ try tool fallbacks) vs a hard error.
enum ResolveErr {
	Auth,
	Other(ClinixError),
}

/// The anonymous OCI-registry flow: HEAD the manifest, do the bearer-token dance
/// on `401`, and read `Docker-Content-Digest`.
fn resolve_via_curl(r: &ImageRef) -> std::result::Result<String, ResolveErr> {
	let url = format!("https://{}/v2/{}/manifests/{}", r.registry, r.repo, r.tag);
	let (status, headers) = head(&url, None).map_err(ResolveErr::Other)?;
	let (status, headers) = if status == 401 {
		// Bearer dance: parse the challenge, fetch a token, retry.
		let challenge = headers
			.get("www-authenticate")
			.ok_or_else(|| ResolveErr::Other(config("401 without a WWW-Authenticate challenge")))?;
		let token = match fetch_token(challenge) {
			Ok(Some(t)) => t,
			Ok(None) => return Err(ResolveErr::Auth), // needs credentials
			Err(e) => return Err(ResolveErr::Other(e)),
		};
		head(&url, Some(&token)).map_err(ResolveErr::Other)?
	} else {
		(status, headers)
	};

	if status == 401 || status == 403 {
		return Err(ResolveErr::Auth);
	}
	if status != 200 {
		return Err(ResolveErr::Other(config(&format!(
			"registry returned HTTP {status} for {url}"
		))));
	}
	headers
		.get("docker-content-digest")
		.filter(|d| d.starts_with("sha256:"))
		.cloned()
		.ok_or_else(|| ResolveErr::Other(config("registry gave no Docker-Content-Digest header")))
}

/// `HEAD <url>` via curl, returning `(status, lowercased-headers)`. Sends the
/// manifest `Accept` and an optional bearer token.
fn head(url: &str, token: Option<&str>) -> Result<(u16, HashMap<String, String>)> {
	let mut cmd = Command::new("curl");
	cmd.args(["-sS", "-I", "-H", &format!("Accept: {ACCEPT}")]);
	if let Some(t) = token {
		cmd.args(["-H", &format!("Authorization: Bearer {t}")]);
	}
	cmd.arg(url);
	let out = cmd
		.output()
		.map_err(|e| config(&format!("curl failed to run (is it installed?): {e}")))?;
	if !out.status.success() && out.stdout.is_empty() {
		return Err(config(&format!(
			"curl error for {url}: {}",
			String::from_utf8_lossy(&out.stderr).trim()
		)));
	}
	Ok(parse_headers(&String::from_utf8_lossy(&out.stdout)))
}

/// Parse a curl `-I` header dump into `(status, headers)`. Header names are
/// lowercased; on a redirect chain the last status/headers win. Pure — testable.
pub fn parse_headers(text: &str) -> (u16, HashMap<String, String>) {
	let mut status = 0u16;
	let mut headers = HashMap::new();
	for line in text.lines() {
		let line = line.trim_end();
		if let Some(rest) = line.strip_prefix("HTTP/") {
			// e.g. "HTTP/2 401" or "HTTP/1.1 200 OK" → the numeric code.
			if let Some(code) = rest.split_whitespace().nth(1).and_then(|c| c.parse().ok()) {
				status = code;
				headers.clear(); // start of a fresh response block
			}
		} else if let Some((k, v)) = line.split_once(':') {
			headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
		}
	}
	(status, headers)
}

/// Parse a `WWW-Authenticate: Bearer realm="…",service="…",scope="…"` challenge
/// into `(realm, service, scope)`. Pure — testable.
pub fn parse_challenge(value: &str) -> Option<(String, String, Option<String>)> {
	let params = value.trim().strip_prefix("Bearer ")?;
	let mut map = HashMap::new();
	for part in params.split(',') {
		if let Some((k, v)) = part.split_once('=') {
			map.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
		}
	}
	let realm = map.remove("realm")?;
	let service = map.remove("service").unwrap_or_default();
	let scope = map.remove("scope");
	Some((realm, service, scope))
}

/// Fetch an anonymous bearer token from the challenge's realm. `Ok(None)` means
/// the token endpoint refused anonymous access (→ needs credentials).
fn fetch_token(challenge: &str) -> Result<Option<String>> {
	let (realm, service, scope) = parse_challenge(challenge)
		.ok_or_else(|| config("unparseable WWW-Authenticate challenge"))?;
	let mut cmd = Command::new("curl");
	cmd.args(["-sS", "-G", &realm]);
	cmd.args(["--data-urlencode", &format!("service={service}")]);
	if let Some(s) = &scope {
		cmd.args(["--data-urlencode", &format!("scope={s}")]);
	}
	let out = cmd
		.output()
		.map_err(|e| config(&format!("curl (token) failed: {e}")))?;
	let body = String::from_utf8_lossy(&out.stdout);
	let json: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
	// Docker Hub uses "token"; some registries "access_token".
	let token = json
		.get("token")
		.or_else(|| json.get("access_token"))
		.and_then(|t| t.as_str())
		.map(str::to_string);
	Ok(token)
}

/// `skopeo inspect` — daemon-free, honours docker credentials for private repos.
fn resolve_via_skopeo(image: &str) -> Option<String> {
	let out = Command::new("skopeo")
		.args([
			"inspect",
			"--format",
			"{{.Digest}}",
			&format!("docker://{image}"),
		])
		.output()
		.ok()?;
	if !out.status.success() {
		return None;
	}
	let d = String::from_utf8_lossy(&out.stdout).trim().to_string();
	d.starts_with("sha256:").then_some(d)
}

/// `docker pull` + inspect `RepoDigests` — the heavy `--pull` fallback.
fn resolve_via_docker_pull(image: &str) -> Option<String> {
	if !Command::new("docker")
		.args(["pull", image])
		.status()
		.ok()?
		.success()
	{
		return None;
	}
	let out = Command::new("docker")
		.args(["inspect", "--format", "{{index .RepoDigests 0}}", image])
		.output()
		.ok()?;
	// e.g. "ubuntu@sha256:abc…" → the digest.
	String::from_utf8_lossy(&out.stdout)
		.trim()
		.rsplit_once('@')
		.map(|(_, d)| d.to_string())
		.filter(|d| d.starts_with("sha256:"))
}

/// Whether an external tool is on PATH (runs `<tool> --version`).
fn tool_present(name: &str) -> bool {
	Command::new(name)
		.arg("--version")
		.output()
		.map(|o| o.status.success())
		.unwrap_or(false)
}

fn config(msg: &str) -> ClinixError {
	ClinixError::Config(msg.to_string())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parse_ref_docker_hub_short_names() {
		let r = parse_ref("ubuntu");
		assert_eq!(r.registry, DOCKER_HUB);
		assert_eq!(r.repo, "library/ubuntu");
		assert_eq!(r.tag, "latest");
		assert_eq!(r.display, "ubuntu");
		assert!(r.digest.is_none());

		let r = parse_ref("ubuntu:22.04");
		assert_eq!(r.repo, "library/ubuntu");
		assert_eq!(r.tag, "22.04");
		assert_eq!(r.display, "ubuntu");
	}

	#[test]
	fn parse_ref_org_image_and_explicit_registry() {
		// A two-part Docker Hub name is NOT library-prefixed.
		let r = parse_ref("nvidia/cuda:12.4.1-runtime-ubuntu22.04");
		assert_eq!(r.registry, DOCKER_HUB);
		assert_eq!(r.repo, "nvidia/cuda");
		assert_eq!(r.tag, "12.4.1-runtime-ubuntu22.04");
		assert_eq!(r.display, "nvidia/cuda");

		// A host with a dot is the registry (and a port is kept).
		let r = parse_ref("nvcr.io/nvidia/pytorch:24.01-py3");
		assert_eq!(r.registry, "nvcr.io");
		assert_eq!(r.repo, "nvidia/pytorch");
		assert_eq!(r.tag, "24.01-py3");
		assert_eq!(r.display, "nvcr.io/nvidia/pytorch");

		let r = parse_ref("localhost:5000/app:dev");
		assert_eq!(r.registry, "localhost:5000");
		assert_eq!(r.repo, "app");
		assert_eq!(r.tag, "dev");
	}

	#[test]
	fn parse_ref_already_digest_pinned() {
		let r = parse_ref("ghcr.io/owner/img@sha256:abc123");
		assert_eq!(r.registry, "ghcr.io");
		assert_eq!(r.repo, "owner/img");
		assert_eq!(r.digest.as_deref(), Some("sha256:abc123"));
	}

	#[test]
	fn pin_reference_keeps_an_existing_digest() {
		let pinned = pin_reference("nvcr.io/nvidia/pytorch:24.01@sha256:deadbeef", false).unwrap();
		assert_eq!(pinned, "nvcr.io/nvidia/pytorch:24.01@sha256:deadbeef");
	}

	#[test]
	fn parse_headers_reads_status_and_digest() {
		let dump = "HTTP/2 401\r\nwww-authenticate: Bearer realm=\"x\"\r\n\r\nHTTP/2 200 OK\r\nDocker-Content-Digest: sha256:abc\r\nContent-Type: application/json\r\n";
		let (status, h) = parse_headers(dump);
		assert_eq!(status, 200, "last block wins");
		assert_eq!(h.get("docker-content-digest").unwrap(), "sha256:abc");
		assert!(h.get("www-authenticate").is_none(), "cleared on new block");
	}

	#[test]
	fn parse_challenge_extracts_realm_service_scope() {
		let v = "Bearer realm=\"https://auth.docker.io/token\",service=\"registry.docker.io\",scope=\"repository:library/ubuntu:pull\"";
		let (realm, service, scope) = parse_challenge(v).unwrap();
		assert_eq!(realm, "https://auth.docker.io/token");
		assert_eq!(service, "registry.docker.io");
		assert_eq!(scope.as_deref(), Some("repository:library/ubuntu:pull"));
	}
}
