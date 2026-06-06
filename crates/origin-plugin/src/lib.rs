// SPDX-License-Identifier: Apache-2.0
//! Plugin manifests, dependency resolution, and live cross-tool skill discovery.
//!
//! Parses plugin manifests, resolves install order topologically, estimates
//! the context-window cost of a plugin's declared surface, and discovers live
//! `.claude` and `.agents` skills on disk. The manifest parser understands a
//! deliberately small TOML subset (top-level string keys and string arrays) so
//! the crate stays dependency-light and compatible with the workspace MSRV.
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::iter::Peekable;
use std::str::Chars;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use walkdir::WalkDir;

/// Errors that can arise while handling plugins.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PluginError {
    /// The manifest source could not be parsed.
    #[error("manifest parse error: {0}")]
    Toml(String),
    /// A dependency cycle was detected during resolution.
    #[error("dependency cycle: {0}")]
    Cycle(String),
    /// A declared dependency refers to an unknown plugin.
    #[error("missing dependency: {0}")]
    Missing(String),
    /// A filesystem operation failed during discovery.
    #[error("io error: {0}")]
    Io(String),
}

/// A parsed plugin manifest describing a plugin's surface and dependencies.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Unique plugin name.
    pub name: String,
    /// Plugin version string (opaque; not interpreted).
    pub version: String,
    /// Slash-command identifiers the plugin contributes.
    pub commands: Vec<String>,
    /// Agent identifiers the plugin contributes.
    pub agents: Vec<String>,
    /// Skill identifiers the plugin contributes.
    pub skills: Vec<String>,
    /// Hook identifiers the plugin registers.
    pub hooks: Vec<String>,
    /// MCP server identifiers the plugin wires up.
    pub mcp: Vec<String>,
    /// LSP server identifiers the plugin wires up.
    pub lsp: Vec<String>,
    /// Names of other plugins this plugin depends on.
    pub deps: Vec<String>,
}

/// Per-kind token weights used by [`context_cost_estimate`].
mod weights {
    /// A command is a compact surface item.
    pub const COMMAND: u32 = 40;
    /// An agent carries a system prompt and is heavier.
    pub const AGENT: u32 = 120;
    /// A skill ships instructions plus metadata.
    pub const SKILL: u32 = 90;
    /// A hook is a small declaration.
    pub const HOOK: u32 = 25;
    /// An MCP server advertises tool schemas.
    pub const MCP: u32 = 150;
    /// An LSP server advertises capabilities.
    pub const LSP: u32 = 60;
}

/// A skill discovered live on disk under a `.claude` or `.agents` tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredSkill {
    /// The skill directory name (the folder containing `SKILL.md`).
    pub name: String,
    /// The absolute or root-relative path to the `SKILL.md` file.
    pub path: String,
    /// Provenance: `".claude"` or `".agents"`.
    pub source: String,
}

/// Parses a plugin manifest from a TOML source string.
///
/// The recognised TOML subset is: blank lines, `#` comments, top-level
/// `key = "value"` string assignments, and top-level `key = ["a", "b"]`
/// string-array assignments (which may span multiple lines). Unknown keys are
/// ignored so newer manifests remain forward-compatible.
///
/// # Errors
///
/// Returns [`PluginError::Toml`] if a value is malformed (for example an
/// unterminated string or array, or a non-string array element).
pub fn parse_manifest(toml_src: &str) -> Result<Manifest, PluginError> {
    let mut manifest = Manifest::default();
    for (key, value) in parse_pairs(toml_src)? {
        match key.as_str() {
            "name" => manifest.name = expect_string(&key, &value)?,
            "version" => manifest.version = expect_string(&key, &value)?,
            "commands" => manifest.commands = expect_array(&key, &value)?,
            "agents" => manifest.agents = expect_array(&key, &value)?,
            "skills" => manifest.skills = expect_array(&key, &value)?,
            "hooks" => manifest.hooks = expect_array(&key, &value)?,
            "mcp" => manifest.mcp = expect_array(&key, &value)?,
            "lsp" => manifest.lsp = expect_array(&key, &value)?,
            "deps" => manifest.deps = expect_array(&key, &value)?,
            _ => {}
        }
    }
    Ok(manifest)
}

/// A raw parsed value: either a scalar string or an array of strings.
enum Value {
    Scalar(String),
    Array(Vec<String>),
}

fn expect_string(key: &str, value: &Value) -> Result<String, PluginError> {
    match value {
        Value::Scalar(s) => Ok(s.clone()),
        Value::Array(_) => Err(PluginError::Toml(format!("key `{key}` expected a string"))),
    }
}

fn expect_array(key: &str, value: &Value) -> Result<Vec<String>, PluginError> {
    match value {
        Value::Array(items) => Ok(items.clone()),
        Value::Scalar(_) => Err(PluginError::Toml(format!("key `{key}` expected an array"))),
    }
}

/// Tokenises the supported TOML subset into ordered `(key, value)` pairs.
fn parse_pairs(src: &str) -> Result<Vec<(String, Value)>, PluginError> {
    let mut pairs = Vec::new();
    let mut chars = src.chars().peekable();
    loop {
        skip_insignificant(&mut chars);
        if chars.peek().is_none() {
            break;
        }
        let key = read_key(&mut chars)?;
        skip_inline_ws(&mut chars);
        match chars.next() {
            Some('=') => {}
            other => {
                return Err(PluginError::Toml(format!(
                    "expected `=` after key `{key}`, found {other:?}"
                )));
            }
        }
        skip_inline_ws(&mut chars);
        let value = read_value(&key, &mut chars)?;
        pairs.push((key, value));
    }
    Ok(pairs)
}

/// Skips whitespace, newlines, and `#` comments.
fn skip_insignificant(chars: &mut Peekable<Chars<'_>>) {
    while let Some(&c) = chars.peek() {
        if c == '#' {
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
        } else if c.is_whitespace() {
            chars.next();
        } else {
            break;
        }
    }
}

/// Skips only spaces and tabs (not newlines).
fn skip_inline_ws(chars: &mut Peekable<Chars<'_>>) {
    while let Some(&c) = chars.peek() {
        if c == ' ' || c == '\t' {
            chars.next();
        } else {
            break;
        }
    }
}

fn read_key(chars: &mut Peekable<Chars<'_>>) -> Result<String, PluginError> {
    let mut key = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_alphanumeric() || c == '_' || c == '-' || c == '.' {
            key.push(c);
            chars.next();
        } else {
            break;
        }
    }
    if key.is_empty() {
        return Err(PluginError::Toml("expected a key".to_owned()));
    }
    Ok(key)
}

fn read_value(key: &str, chars: &mut Peekable<Chars<'_>>) -> Result<Value, PluginError> {
    match chars.peek() {
        Some('"') => Ok(Value::Scalar(read_string(chars)?)),
        Some('[') => Ok(Value::Array(read_array(chars)?)),
        other => Err(PluginError::Toml(format!(
            "key `{key}` has unsupported value starting with {other:?}"
        ))),
    }
}

/// Reads a double-quoted string with basic backslash escapes.
fn read_string(chars: &mut Peekable<Chars<'_>>) -> Result<String, PluginError> {
    // Consume the opening quote.
    chars.next();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Ok(out),
            '\\' => {
                let escaped = chars
                    .next()
                    .ok_or_else(|| PluginError::Toml("unterminated escape in string".to_owned()))?;
                match escaped {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    '\\' => out.push('\\'),
                    '"' => out.push('"'),
                    other => out.push(other),
                }
            }
            other => out.push(other),
        }
    }
    Err(PluginError::Toml("unterminated string literal".to_owned()))
}

/// Reads a `[ "a", "b" ]` array of strings, possibly spanning lines.
fn read_array(chars: &mut Peekable<Chars<'_>>) -> Result<Vec<String>, PluginError> {
    // Consume the opening bracket.
    chars.next();
    let mut items = Vec::new();
    loop {
        skip_insignificant(chars);
        match chars.peek() {
            None => return Err(PluginError::Toml("unterminated array".to_owned())),
            Some(']') => {
                chars.next();
                return Ok(items);
            }
            Some('"') => {
                items.push(read_string(chars)?);
                skip_insignificant(chars);
                match chars.peek() {
                    Some(',') => {
                        chars.next();
                    }
                    Some(']') => {
                        chars.next();
                        return Ok(items);
                    }
                    other => {
                        return Err(PluginError::Toml(format!(
                            "expected `,` or `]` in array, found {other:?}"
                        )));
                    }
                }
            }
            other => {
                return Err(PluginError::Toml(format!(
                    "array elements must be strings, found {other:?}"
                )));
            }
        }
    }
}

/// Computes a deterministic topological install order for a set of plugins.
///
/// Each manifest's `deps` must refer to a plugin present in `manifests`. The
/// returned order lists every dependency before the plugins that require it,
/// breaking ties alphabetically for stable output.
///
/// # Errors
///
/// Returns [`PluginError::Missing`] if a dependency names an unknown plugin and
/// [`PluginError::Cycle`] if the dependency graph contains a cycle.
pub fn resolve_order(manifests: &[Manifest]) -> Result<Vec<String>, PluginError> {
    // Build the dependency map. BTreeMap/BTreeSet give deterministic ordering.
    let mut deps_of: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for m in manifests {
        deps_of.entry(m.name.as_str()).or_default();
    }
    for m in manifests {
        for dep in &m.deps {
            if !deps_of.contains_key(dep.as_str()) {
                return Err(PluginError::Missing(format!(
                    "plugin `{}` depends on unknown `{dep}`",
                    m.name
                )));
            }
            deps_of
                .entry(m.name.as_str())
                .or_default()
                .insert(dep.as_str());
        }
    }

    // Kahn's algorithm: repeatedly emit nodes with no unresolved dependencies.
    let mut resolved: Vec<String> = Vec::with_capacity(deps_of.len());
    let mut done: BTreeSet<&str> = BTreeSet::new();
    while done.len() < deps_of.len() {
        let mut progressed = false;
        for (&name, deps) in &deps_of {
            if done.contains(name) {
                continue;
            }
            if deps.iter().all(|d| done.contains(d)) {
                resolved.push(name.to_owned());
                done.insert(name);
                progressed = true;
            }
        }
        if !progressed {
            let mut remaining: Vec<&str> = deps_of
                .keys()
                .filter(|n| !done.contains(*n))
                .copied()
                .collect();
            remaining.sort_unstable();
            return Err(PluginError::Cycle(remaining.join(" -> ")));
        }
    }
    Ok(resolved)
}

/// Estimates the rough context-window token cost of a plugin's surface.
///
/// The estimate is the sum of declared surface items weighted by kind. It is
/// monotonic: adding any surface item never lowers the estimate. Dependencies
/// are excluded because their cost is attributed to their own manifests.
#[must_use]
pub fn context_cost_estimate(m: &Manifest) -> u32 {
    surface_len(&m.commands)
        .saturating_mul(weights::COMMAND)
        .saturating_add(surface_len(&m.agents).saturating_mul(weights::AGENT))
        .saturating_add(surface_len(&m.skills).saturating_mul(weights::SKILL))
        .saturating_add(surface_len(&m.hooks).saturating_mul(weights::HOOK))
        .saturating_add(surface_len(&m.mcp).saturating_mul(weights::MCP))
        .saturating_add(surface_len(&m.lsp).saturating_mul(weights::LSP))
}

/// Returns the length of a surface list saturated into `u32`.
fn surface_len(items: &[String]) -> u32 {
    u32::try_from(items.len()).unwrap_or(u32::MAX)
}

/// Discovers live skills under each root's `.claude` and `.agents` trees.
///
/// For every `root` in `roots`, this scans `<root>/.claude/skills/*/SKILL.md`
/// and `<root>/.agents/skills/*/SKILL.md`, returning one [`DiscoveredSkill`]
/// per `SKILL.md` found. Roots whose `.claude`/`.agents` directories are absent
/// simply contribute nothing, so callers can toggle a source by including or
/// omitting its root. Results are sorted by `(source, name, path)` for
/// determinism.
///
/// # Errors
///
/// Returns [`PluginError::Io`] if traversal of an existing directory fails (for
/// example due to permissions). A missing directory is not an error.
pub fn discover_skills(roots: &[String]) -> Result<Vec<DiscoveredSkill>, PluginError> {
    let mut found = Vec::new();
    for root in roots {
        for (sub, source) in [(".claude", ".claude"), (".agents", ".agents")] {
            let skills_dir = std::path::Path::new(root).join(sub).join("skills");
            if !skills_dir.is_dir() {
                continue;
            }
            collect_from(&skills_dir, source, &mut found)?;
        }
    }
    found.sort_unstable_by(|a, b| {
        (a.source.as_str(), a.name.as_str(), a.path.as_str()).cmp(&(
            b.source.as_str(),
            b.name.as_str(),
            b.path.as_str(),
        ))
    });
    Ok(found)
}

/// Walks a `skills` directory collecting every `SKILL.md` file.
fn collect_from(
    skills_dir: &std::path::Path,
    source: &str,
    out: &mut Vec<DiscoveredSkill>,
) -> Result<(), PluginError> {
    for entry in WalkDir::new(skills_dir).min_depth(2).max_depth(2) {
        let entry = entry.map_err(|e| PluginError::Io(e.to_string()))?;
        if !entry.file_type().is_file() || entry.file_name() != "SKILL.md" {
            continue;
        }
        let path = entry.path();
        let name = path
            .parent()
            .and_then(std::path::Path::file_name)
            .map_or_else(|| "unknown".to_owned(), |n| n.to_string_lossy().into_owned());
        out.push(DiscoveredSkill {
            name,
            path: path.to_string_lossy().into_owned(),
            source: source.to_owned(),
        });
    }
    Ok(())
}

/// Candidate manifest file names, tried in order, when validating a directory.
pub const MANIFEST_NAMES: [&str; 2] = ["plugin.toml", "manifest.toml"];

/// Validates the plugin manifest at `path` and returns the parsed [`Manifest`].
///
/// `path` may point either directly at a manifest file or at a plugin directory
/// containing one of [`MANIFEST_NAMES`]. A manifest is considered valid when it
/// parses cleanly and declares a non-empty `name` (the install destination is
/// derived from it). Unknown keys remain tolerated, matching [`parse_manifest`].
///
/// # Errors
///
/// Returns [`PluginError::Io`] if no manifest file can be read at `path` and
/// [`PluginError::Toml`] if the manifest is malformed or omits `name`.
pub fn validate_manifest_at(path: &std::path::Path) -> Result<Manifest, PluginError> {
    let src = read_manifest_source(path)?;
    let manifest = parse_manifest(&src)?;
    if manifest.name.trim().is_empty() {
        return Err(PluginError::Toml(
            "manifest is missing a non-empty `name`".to_owned(),
        ));
    }
    Ok(manifest)
}

/// `true` when `name` is a single, filesystem-safe path component suitable for
/// joining onto the plugins root. Guards `install_into` against path traversal
/// from a hostile bundle manifest (`..`, `/`, `\\`, absolute paths, NUL, leading
/// dot). Mirrors the conservative character set used for crate-style names.
fn is_safe_plugin_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Reads manifest source from a file path or a directory containing a manifest.
fn read_manifest_source(path: &std::path::Path) -> Result<String, PluginError> {
    if path.is_file() {
        return std::fs::read_to_string(path).map_err(|e| PluginError::Io(e.to_string()));
    }
    if path.is_dir() {
        for name in MANIFEST_NAMES {
            let candidate = path.join(name);
            if candidate.is_file() {
                return std::fs::read_to_string(&candidate)
                    .map_err(|e| PluginError::Io(e.to_string()));
            }
        }
        return Err(PluginError::Io(format!(
            "no plugin manifest ({}) found in {}",
            MANIFEST_NAMES.join(" or "),
            path.display()
        )));
    }
    Err(PluginError::Io(format!("path not found: {}", path.display())))
}

/// Installs the plugin bundle at `src` into `plugins_root/<manifest name>`.
///
/// The source directory tree is copied verbatim (its `.git` directory, if any,
/// is skipped) into a destination named after the validated manifest. Any
/// pre-existing destination is removed first, so re-installing overwrites
/// cleanly and the operation is idempotent. On manifest-validation failure the
/// partially-copied destination is removed before the error is returned, so a
/// failed install never leaves a half-written plugin behind.
///
/// Returns the validated [`Manifest`] together with the destination path.
///
/// # Errors
///
/// Returns [`PluginError::Io`] on any filesystem failure and the manifest error
/// from [`validate_manifest_at`] when the copied bundle has no valid manifest.
pub fn install_into(
    src: &std::path::Path,
    plugins_root: &std::path::Path,
) -> Result<(Manifest, std::path::PathBuf), PluginError> {
    // Validate before copying so a clearly-broken source fails fast and we do
    // not derive a destination name from a missing manifest.
    let manifest = validate_manifest_at(src)?;
    // SECURITY: the destination dir name is derived from the (untrusted) bundle
    // manifest and is then `remove_dir_all`/`create_dir_all`/copied into. Reject
    // any name that is not a single safe path component so a hostile manifest
    // (`name = "../.."`, an absolute path, …) cannot traverse out of the plugins
    // root and delete or overwrite arbitrary files.
    let name = manifest.name.trim();
    if !is_safe_plugin_name(name) {
        return Err(PluginError::Toml(format!(
            "manifest `name` must be a simple identifier ([A-Za-z0-9_-], no path separators or leading dot, <=64 chars); got {name:?}"
        )));
    }
    let dest = plugins_root.join(name);
    // Defense-in-depth: the destination must be a direct child of the plugins
    // root, even if the name check above ever regresses.
    if dest.parent() != Some(plugins_root) {
        return Err(PluginError::Io(
            "refusing to install outside the plugins root".to_owned(),
        ));
    }

    // Idempotent overwrite: clear any prior install at the destination.
    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(|e| PluginError::Io(e.to_string()))?;
    }
    std::fs::create_dir_all(&dest).map_err(|e| PluginError::Io(e.to_string()))?;

    // Copy the tree, then re-validate at the destination as a defensive check.
    if let Err(e) = copy_tree(src, &dest) {
        std::fs::remove_dir_all(&dest).ok();
        return Err(e);
    }
    match validate_manifest_at(&dest) {
        Ok(installed) => Ok((installed, dest)),
        Err(e) => {
            std::fs::remove_dir_all(&dest).ok();
            Err(e)
        }
    }
}

/// Recursively copies the directory tree at `src` into `dest`, skipping `.git`.
fn copy_tree(src: &std::path::Path, dest: &std::path::Path) -> Result<(), PluginError> {
    std::fs::create_dir_all(dest).map_err(|e| PluginError::Io(e.to_string()))?;
    let entries = std::fs::read_dir(src).map_err(|e| PluginError::Io(e.to_string()))?;
    for entry in entries {
        let entry = entry.map_err(|e| PluginError::Io(e.to_string()))?;
        let file_type = entry.file_type().map_err(|e| PluginError::Io(e.to_string()))?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if file_type.is_dir() {
            if entry.file_name() == ".git" {
                continue;
            }
            copy_tree(&from, &to)?;
        } else if file_type.is_file() {
            std::fs::copy(&from, &to).map_err(|e| PluginError::Io(e.to_string()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        dir.push(format!("origin-plugin-{tag}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn manifest(name: &str, deps: &[&str]) -> Manifest {
        Manifest {
            name: name.to_owned(),
            version: "1.0.0".to_owned(),
            deps: deps.iter().map(|d| (*d).to_owned()).collect(),
            ..Manifest::default()
        }
    }

    #[test]
    fn parses_a_full_manifest() {
        let src = r#"
            # a plugin manifest
            name = "fmt"
            version = "0.2.1"
            commands = ["format", "check"]
            agents = ["reviewer"]
            skills = []
            hooks = ["pre-commit"]
            mcp = ["fs-server"]
            lsp = ["rust-analyzer"]
            deps = ["core"]
        "#;
        let m = parse_manifest(src).unwrap();
        assert_eq!(m.name, "fmt");
        assert_eq!(m.version, "0.2.1");
        assert_eq!(m.commands, vec!["format", "check"]);
        assert_eq!(m.agents, vec!["reviewer"]);
        assert!(m.skills.is_empty());
        assert_eq!(m.hooks, vec!["pre-commit"]);
        assert_eq!(m.mcp, vec!["fs-server"]);
        assert_eq!(m.lsp, vec!["rust-analyzer"]);
        assert_eq!(m.deps, vec!["core"]);
    }

    #[test]
    fn parse_rejects_type_mismatch_and_unterminated() {
        assert!(matches!(
            parse_manifest("name = [\"oops\"]"),
            Err(PluginError::Toml(_))
        ));
        assert!(matches!(
            parse_manifest("commands = \"not-an-array\""),
            Err(PluginError::Toml(_))
        ));
        assert!(matches!(
            parse_manifest("name = \"unterminated"),
            Err(PluginError::Toml(_))
        ));
        assert!(matches!(
            parse_manifest("commands = [\"x\""),
            Err(PluginError::Toml(_))
        ));
    }

    #[test]
    fn parse_ignores_unknown_keys() {
        let m = parse_manifest("name = \"x\"\nfuture_field = \"y\"\nversion = \"1\"").unwrap();
        assert_eq!(m.name, "x");
        assert_eq!(m.version, "1");
    }

    #[test]
    fn resolve_order_topo_sorts_chain() {
        // c depends on b, b depends on a => a, b, c.
        let ms = [
            manifest("c", &["b"]),
            manifest("a", &[]),
            manifest("b", &["a"]),
        ];
        let order = resolve_order(&ms).unwrap();
        let pos = |n: &str| order.iter().position(|x| x == n).unwrap();
        assert!(pos("a") < pos("b"));
        assert!(pos("b") < pos("c"));
        assert_eq!(order.len(), 3);
    }

    #[test]
    fn resolve_order_detects_cycle() {
        let ms = [manifest("a", &["b"]), manifest("b", &["a"])];
        assert!(matches!(resolve_order(&ms), Err(PluginError::Cycle(_))));
    }

    #[test]
    fn resolve_order_reports_missing_dep() {
        let ms = [manifest("a", &["ghost"])];
        let err = resolve_order(&ms).unwrap_err();
        assert!(matches!(&err, PluginError::Missing(msg) if msg.contains("ghost")));
    }

    #[test]
    fn cost_estimate_is_monotonic_with_surface() {
        let base = Manifest {
            name: "p".to_owned(),
            version: "1".to_owned(),
            commands: vec!["a".to_owned()],
            ..Manifest::default()
        };
        let base_cost = context_cost_estimate(&base);
        let mut bigger = base.clone();
        bigger.agents.push("agent".to_owned());
        let bigger_cost = context_cost_estimate(&bigger);
        assert!(bigger_cost > base_cost);
        // Empty manifest costs nothing; deps do not contribute.
        let mut with_deps = base;
        with_deps.deps.push("other".to_owned());
        assert_eq!(context_cost_estimate(&with_deps), base_cost);
        assert_eq!(context_cost_estimate(&Manifest::default()), 0);
    }

    #[test]
    fn discover_skills_finds_skill_md_in_both_sources() {
        let root = temp_dir("discover");
        let claude_skill = root.join(".claude").join("skills").join("alpha");
        let agents_skill = root.join(".agents").join("skills").join("beta");
        fs::create_dir_all(&claude_skill).unwrap();
        fs::create_dir_all(&agents_skill).unwrap();
        fs::write(claude_skill.join("SKILL.md"), "# alpha").unwrap();
        fs::write(agents_skill.join("SKILL.md"), "# beta").unwrap();
        // A non-SKILL file should be ignored.
        fs::write(claude_skill.join("README.md"), "nope").unwrap();

        let roots = vec![root.to_string_lossy().into_owned()];
        let skills = discover_skills(&roots).unwrap();
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].source, ".agents");
        assert_eq!(skills[0].name, "beta");
        assert_eq!(skills[1].source, ".claude");
        assert_eq!(skills[1].name, "alpha");
        assert!(skills[1].path.ends_with("SKILL.md"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn discover_skills_tolerates_missing_roots() {
        let skills = discover_skills(&["/nonexistent/origin/plugin/root".to_owned()]).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn validate_manifest_at_accepts_wellformed_and_rejects_malformed() {
        let root = temp_dir("validate");

        // Well-formed bundle directory with a manifest file.
        let good = root.join("good");
        fs::create_dir_all(&good).unwrap();
        fs::write(
            good.join("plugin.toml"),
            "name = \"fmt\"\nversion = \"0.1.0\"\ncommands = [\"format\"]\n",
        )
        .unwrap();
        let m = validate_manifest_at(&good).unwrap();
        assert_eq!(m.name, "fmt");
        assert_eq!(m.commands, vec!["format"]);

        // Malformed manifest: type mismatch is rejected as a Toml error.
        let bad = root.join("bad");
        fs::create_dir_all(&bad).unwrap();
        fs::write(bad.join("plugin.toml"), "name = [\"oops\"]\n").unwrap();
        assert!(matches!(
            validate_manifest_at(&bad),
            Err(PluginError::Toml(_))
        ));

        // Missing-name manifest is rejected even though it parses.
        let nameless = root.join("nameless");
        fs::create_dir_all(&nameless).unwrap();
        fs::write(nameless.join("plugin.toml"), "version = \"1\"\n").unwrap();
        assert!(matches!(
            validate_manifest_at(&nameless),
            Err(PluginError::Toml(_))
        ));

        // Directory with no manifest at all is an Io error.
        let empty = root.join("empty");
        fs::create_dir_all(&empty).unwrap();
        assert!(matches!(validate_manifest_at(&empty), Err(PluginError::Io(_))));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn install_into_copies_local_bundle_and_is_idempotent() {
        let root = temp_dir("install");
        let src = root.join("src-bundle");
        let nested = src.join("commands");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            src.join("plugin.toml"),
            "name = \"demo\"\nversion = \"2.0\"\nagents = [\"a\", \"b\"]\n",
        )
        .unwrap();
        fs::write(nested.join("hello.md"), "# hello").unwrap();
        // A .git directory must not be copied into the install.
        let git = src.join(".git");
        fs::create_dir_all(&git).unwrap();
        fs::write(git.join("config"), "[core]").unwrap();

        let plugins_root = root.join("plugins");
        let (manifest, dest) = install_into(&src, &plugins_root).unwrap();
        assert_eq!(manifest.name, "demo");
        assert_eq!(manifest.agents, vec!["a", "b"]);
        assert_eq!(dest, plugins_root.join("demo"));
        assert!(dest.join("plugin.toml").is_file());
        assert!(dest.join("commands").join("hello.md").is_file());
        assert!(!dest.join(".git").exists());

        // Re-installing overwrites cleanly: a stale file from a prior install is
        // gone after a fresh install.
        fs::write(dest.join("stale.txt"), "old").unwrap();
        let (again, dest2) = install_into(&src, &plugins_root).unwrap();
        assert_eq!(again.name, "demo");
        assert_eq!(dest2, dest);
        assert!(!dest.join("stale.txt").exists());
        assert!(dest.join("plugin.toml").is_file());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn install_into_rejects_invalid_manifest_without_leaving_dir() {
        let root = temp_dir("install-bad");
        let src = root.join("bad-bundle");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("plugin.toml"), "commands = \"not-an-array\"\n").unwrap();

        let plugins_root = root.join("plugins");
        let err = install_into(&src, &plugins_root).unwrap_err();
        assert!(matches!(err, PluginError::Toml(_)));
        // Fast-fail before any destination is created.
        assert!(!plugins_root.join("bad-bundle").exists());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn install_into_rejects_path_traversal_name() {
        // SECURITY regression: a hostile bundle manifest whose `name` tries to
        // escape the plugins root must be rejected before any filesystem side
        // effect, so it cannot delete/overwrite arbitrary files.
        let root = temp_dir("install-traversal");
        let src = root.join("evil-bundle");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("plugin.toml"),
            "name = \"../../etc\"\nversion = \"1.0\"\n",
        )
        .unwrap();
        let plugins_root = root.join("plugins");
        fs::create_dir_all(&plugins_root).unwrap();

        let err = install_into(&src, &plugins_root).unwrap_err();
        assert!(matches!(err, PluginError::Toml(_)), "traversal name must be rejected");
        // The plugins root is untouched — nothing created or removed.
        assert!(
            plugins_root.read_dir().unwrap().next().is_none(),
            "plugins root must stay empty after a rejected traversal install"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn is_safe_plugin_name_accepts_simple_rejects_dangerous() {
        assert!(is_safe_plugin_name("demo"));
        assert!(is_safe_plugin_name("my-plugin_2"));
        assert!(!is_safe_plugin_name(""));
        assert!(!is_safe_plugin_name("../etc"));
        assert!(!is_safe_plugin_name("a/b"));
        assert!(!is_safe_plugin_name("a\\b"));
        assert!(!is_safe_plugin_name(".hidden"));
        assert!(!is_safe_plugin_name("has space"));
    }
}
