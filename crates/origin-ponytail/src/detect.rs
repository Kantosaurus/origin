// SPDX-License-Identifier: Apache-2.0
//! Deterministic dependency-addition detection. Fails open: any parse error
//! yields no deps (never a false block).

use std::collections::BTreeSet;

use crate::native_table::Ecosystem;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dep {
    pub eco: Ecosystem,
    pub name: String,
}

fn dep(eco: Ecosystem, name: impl Into<String>) -> Dep {
    Dep { eco, name: name.into() }
}

/// Strip a version/spec suffix and surrounding quotes from a token.
fn bare_name(tok: &str) -> String {
    let t = tok.trim().trim_matches('"').trim_matches('\'');
    // npm `pkg@1.2` (but keep scoped `@scope/pkg`), python `pkg==1`, `pkg>=1`.
    let cut = if let Some(stripped) = t.strip_prefix('@') {
        // scoped: only split a SECOND '@'
        stripped.find('@').map(|i| i + 1)
    } else {
        t.find(['@', '=', '>', '<', '~', '!', '[', ';', ',', ' ', '"', '\''])
    };
    match cut {
        Some(i) => t[..i].to_string(),
        None => t.to_string(),
    }
}

/// Parse package-manager install commands. Only the FIRST simple command is
/// inspected (head token must be the package manager). Returns deps only when
/// explicit package names follow the install verb.
#[must_use]
pub fn bash_installs(cmd: &str) -> Vec<Dep> {
    let cmd = cmd.trim();
    let toks: Vec<&str> = cmd.split_whitespace().collect();
    if toks.len() < 3 {
        return Vec::new();
    }
    let (eco, verb_ok, start) = match toks[0] {
        "npm" | "yarn" | "pnpm" | "bun" => {
            (Ecosystem::Npm, matches!(toks[1], "install" | "add" | "i"), 2)
        }
        "cargo" => (Ecosystem::Cargo, toks[1] == "add", 2),
        "go" => (Ecosystem::Go, toks[1] == "get", 2),
        "gem" => (Ecosystem::RubyGems, toks[1] == "install", 2),
        "pip" | "pip3" => (Ecosystem::PyPI, toks[1] == "install", 2),
        "uv" => (Ecosystem::PyPI, toks.get(1) == Some(&"pip") && toks.get(2) == Some(&"install"), 3),
        "poetry" => (Ecosystem::PyPI, toks[1] == "add", 2),
        _ => return Vec::new(),
    };
    if !verb_ok {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for tok in &toks[start..] {
        if tok.starts_with('-') {
            continue; // flag
        }
        if *tok == "-r" {
            continue;
        }
        let name = bare_name(tok);
        if name.is_empty() || name == "requirements.txt" {
            continue;
        }
        if seen.insert(name.clone()) {
            out.push(dep(eco, name));
        }
    }
    out
}

fn ecosystem_for(file: &str) -> Option<Ecosystem> {
    let base = file.rsplit(['/', '\\']).next().unwrap_or(file);
    match base {
        "package.json" => Some(Ecosystem::Npm),
        "Cargo.toml" => Some(Ecosystem::Cargo),
        "requirements.txt" | "pyproject.toml" => Some(Ecosystem::PyPI),
        "go.mod" => Some(Ecosystem::Go),
        "Gemfile" => Some(Ecosystem::RubyGems),
        _ => None,
    }
}

fn deps_in_manifest(file: &str, content: &str) -> BTreeSet<String> {
    let Some(eco) = ecosystem_for(file) else { return BTreeSet::new() };
    let base = file.rsplit(['/', '\\']).next().unwrap_or(file);
    let mut set = BTreeSet::new();
    match base {
        "package.json" => {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(content) {
                for key in ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"] {
                    if let Some(obj) = v.get(key).and_then(|x| x.as_object()) {
                        set.extend(obj.keys().cloned());
                    }
                }
            }
        }
        "Cargo.toml" => {
            if let Ok(v) = content.parse::<toml::Value>() {
                for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
                    if let Some(t) = v.get(key).and_then(|x| x.as_table()) {
                        set.extend(t.keys().cloned());
                    }
                }
            }
        }
        "pyproject.toml" => {
            if let Ok(v) = content.parse::<toml::Value>() {
                if let Some(arr) = v.get("project").and_then(|p| p.get("dependencies")).and_then(|d| d.as_array()) {
                    for item in arr {
                        if let Some(s) = item.as_str() { set.insert(bare_name(s)); }
                    }
                }
                if let Some(t) = v.get("tool").and_then(|t| t.get("poetry")).and_then(|p| p.get("dependencies")).and_then(|d| d.as_table()) {
                    set.extend(t.keys().filter(|k| *k != "python").cloned());
                }
            }
        }
        "requirements.txt" | "Gemfile" | "go.mod" => {
            for d in deps_in_lines(eco, content) {
                set.insert(d.name);
            }
        }
        _ => {}
    }
    set
}

fn deps_in_lines(eco: Ecosystem, text: &str) -> Vec<Dep> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        match eco {
            Ecosystem::PyPI => {
                if line.starts_with('-') { continue; }
                let name = bare_name(line);
                if !name.is_empty() { out.push(dep(eco, name)); }
            }
            Ecosystem::RubyGems => {
                if let Some(rest) = line.strip_prefix("gem ") {
                    let name = bare_name(rest.trim().trim_start_matches([' ', '"', '\'']));
                    if !name.is_empty() { out.push(dep(eco, name)); }
                }
            }
            Ecosystem::Go => {
                // `require x v1` or a line inside a require( ) block: `x v1`.
                let l = line.strip_prefix("require ").unwrap_or(line);
                if let Some(path) = l.split_whitespace().next() {
                    if path.contains('.') && path.contains('/') {
                        out.push(dep(eco, path.to_string()));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Diff a manifest's dep set (full-content). `before == None` ⇒ all deps are added.
#[must_use]
pub fn manifest_deps_added(file: &str, before: Option<&str>, after: &str) -> Vec<Dep> {
    let Some(eco) = ecosystem_for(file) else { return Vec::new() };
    let after_set = deps_in_manifest(file, after);
    let before_set = before.map(|b| deps_in_manifest(file, b)).unwrap_or_default();
    after_set.difference(&before_set).map(|n| dep(eco, n.clone())).collect()
}

/// Scan only newly-inserted text (an Edit's new_string, or `+` patch lines) for
/// dependency declarations. Conservative line/JSON-entry patterns; fails open.
#[must_use]
pub fn manifest_deps_in_added_lines(file: &str, added: &str) -> Vec<Dep> {
    let Some(eco) = ecosystem_for(file) else { return Vec::new() };
    let base = file.rsplit(['/', '\\']).next().unwrap_or(file);
    match base {
        "requirements.txt" | "Gemfile" | "go.mod" => deps_in_lines(eco, added),
        "package.json" => {
            // Match `"name": "version"` entries in the inserted fragment.
            let mut out = Vec::new();
            for line in added.lines() {
                let l = line.trim().trim_end_matches(',');
                if let Some((k, v)) = l.split_once(':') {
                    let key = k.trim().trim_matches('"');
                    let val = v.trim();
                    if val.starts_with('"') && !key.is_empty() && !key.contains(' ') {
                        out.push(dep(eco, key.to_string()));
                    }
                }
            }
            out
        }
        "Cargo.toml" => {
            let mut out = Vec::new();
            for line in added.lines() {
                let l = line.trim();
                if let Some((k, v)) = l.split_once('=') {
                    let key = k.trim();
                    if !key.is_empty() && !key.starts_with('[') && (v.contains('"') || v.contains('{')) {
                        out.push(dep(eco, key.to_string()));
                    }
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[Dep]) -> Vec<&str> { v.iter().map(|d| d.name.as_str()).collect() }

    #[test]
    fn bash_install_named_packages() {
        assert_eq!(names(&bash_installs("npm install lodash")), ["lodash"]);
        assert_eq!(names(&bash_installs("yarn add lodash react")), ["lodash", "react"]);
        assert_eq!(names(&bash_installs("pnpm add -D typescript")), ["typescript"]);
        assert_eq!(names(&bash_installs("cargo add serde@1")), ["serde"]);
        assert_eq!(names(&bash_installs("pip install requests==2.31")), ["requests"]);
        assert_eq!(names(&bash_installs("go get github.com/pkg/errors")), ["github.com/pkg/errors"]);
        assert_eq!(names(&bash_installs("gem install rest-client")), ["rest-client"]);
        assert_eq!(names(&bash_installs("npm i @scope/pkg")), ["@scope/pkg"]);
    }

    #[test]
    fn bash_install_without_named_packages_is_empty() {
        // Installing existing manifest deps is NOT adding a dep.
        assert!(bash_installs("npm install").is_empty());
        assert!(bash_installs("yarn").is_empty());
        assert!(bash_installs("pnpm i").is_empty());
        assert!(bash_installs("pip install -r requirements.txt").is_empty());
        assert!(bash_installs("cargo build").is_empty());
        assert!(bash_installs("go build ./...").is_empty());
        assert!(bash_installs("go mod download").is_empty());
        assert!(bash_installs("echo npm install lodash").is_empty()); // not an install verb at head
    }

    #[test]
    fn manifest_added_diff() {
        let before = r#"{"dependencies":{"react":"18"}}"#;
        let after = r#"{"dependencies":{"react":"18","lodash":"4"}}"#;
        assert_eq!(names(&manifest_deps_added("package.json", Some(before), after)), ["lodash"]);
    }

    #[test]
    fn manifest_new_file_all_added() {
        let after = "[dependencies]\nlazy_static = \"1\"\n";
        assert_eq!(names(&manifest_deps_added("Cargo.toml", None, after)), ["lazy_static"]);
    }

    #[test]
    fn manifest_requirements_and_gemfile_lines() {
        assert_eq!(names(&manifest_deps_in_added_lines("requirements.txt", "pytz==2024.1\n# comment\n")), ["pytz"]);
        assert_eq!(names(&manifest_deps_in_added_lines("Gemfile", "gem \"rest-client\", \"~> 2.0\"")), ["rest-client"]);
    }
}
