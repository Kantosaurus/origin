// SPDX-License-Identifier: Apache-2.0
//! C6: the dependency gate. A `Bash` command that installs a package the
//! project does not already declare needs approval (the command guard's
//! `NeedsApproval`), with a reason naming the package.
//!
//! `origin_cmdparse::installs` finds the installs (through wrappers, `bash -c`
//! and chains); this module checks each named package against the manifests
//! from the command's working directory up to the workspace root. A package
//! from a URL, repository, tarball or path, a global install, and a manifest
//! that is missing or cannot be parsed all need approval (fail closed).

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use origin_cmdparse::Ecosystem;

/// Why `cmd`, run in `cwd` (relative to `root`, the workspace root) needs
/// approval as an install of undeclared packages; `None` when it installs
/// nothing new.
#[must_use]
pub fn reason(cmd: &str, cwd: Option<&str>, root: &Path) -> Option<String> {
    let installs = origin_cmdparse::installs(cmd);
    if installs.is_empty() {
        return None;
    }
    let dir = cwd.map_or_else(|| root.to_path_buf(), |c| root.join(c));
    let mut declared: HashMap<Ecosystem, Result<HashSet<String>, String>> = HashMap::new();
    let mut flagged: Vec<String> = Vec::new();
    for ins in &installs {
        for pkg in &ins.packages {
            // `pip install .`, `go get ./...`: the project itself, not a dependency.
            if pkg == "." || (ins.ecosystem == Ecosystem::Go && pkg.starts_with('.')) {
                continue;
            }
            if ins.global {
                flagged.push(format!("{pkg} (global install)"));
                continue;
            }
            if unknown_source(ins.ecosystem, pkg) {
                flagged.push(format!("{pkg} (from a URL, repository or path)"));
                continue;
            }
            let name = package_name(ins.ecosystem, pkg);
            let names = declared
                .entry(ins.ecosystem)
                .or_insert_with(|| declared_packages(ins.ecosystem, &dir, root));
            match names {
                Ok(set) if set.contains(&name) => {}
                Ok(_) => flagged.push(name),
                Err(why) => flagged.push(format!("{name} ({why})")),
            }
        }
    }
    (!flagged.is_empty()).then(|| {
        format!(
            "installs a package the project does not declare: {}",
            flagged.join(", ")
        )
    })
}

/// A package argument that is not a registry name: a URL, VCS spec, path,
/// tarball or wheel (npm `user/repo` is GitHub shorthand).
fn unknown_source(eco: Ecosystem, pkg: &str) -> bool {
    let a = pkg.to_ascii_lowercase();
    a.contains("://")
        || ["git+", "git@", "git:", "github:", "file:", "link:"]
            .iter()
            .any(|p| a.starts_with(p))
        || a.starts_with(['/', '\\', '~', '.'])
        || a.as_bytes().get(1) == Some(&b':')
        || [".tgz", ".tar.gz", ".tar", ".zip", ".whl"]
            .iter()
            .any(|s| a.ends_with(s))
        || match eco {
            Ecosystem::Npm => !a.starts_with('@') && a.contains('/'),
            Ecosystem::Go => a.contains('\\'),
            Ecosystem::Cargo | Ecosystem::Python => a.contains(['/', '\\']),
        }
}

/// The package name with its version / extras stripped (`left-pad@1.2.3`,
/// `@scope/name@^2`, `serde@1`, `requests[socks]>=2`, `example.com/mod@v1`).
fn package_name(eco: Ecosystem, pkg: &str) -> String {
    let before_at = |s: &str| s.split('@').next().unwrap_or(s).to_string();
    match eco {
        Ecosystem::Python => pep503(pkg),
        Ecosystem::Npm => pkg
            .strip_prefix('@')
            .map_or_else(|| before_at(pkg), |rest| format!("@{}", before_at(rest))),
        Ecosystem::Cargo | Ecosystem::Go => before_at(pkg),
    }
}

/// The leading distribution name of a requirement, PEP 503-normalised
/// (lower-case, runs of `-`, `_`, `.` folded to `-`).
fn pep503(req: &str) -> String {
    let mut out = String::new();
    for c in req
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out
}

/// Every package declared for `eco` in the manifests from `dir` up to `root`.
/// `Err` when none exists or one cannot be read or parsed.
fn declared_packages(eco: Ecosystem, dir: &Path, root: &Path) -> Result<HashSet<String>, String> {
    if dir.components().any(|c| c == Component::ParentDir) {
        return Err("working directory is outside the workspace".into());
    }
    let mut names = HashSet::new();
    let mut found = false;
    for d in dir.ancestors().take_while(|d| d.starts_with(root)) {
        for f in manifest_files(eco, d) {
            let text = match std::fs::read_to_string(&f) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("cannot read {}: {e}", file_name(&f))),
            };
            found = true;
            parse_manifest(eco, &f, &text, &mut names)
                .map_err(|e| format!("cannot parse {}: {e}", file_name(&f)))?;
        }
    }
    if found {
        Ok(names)
    } else {
        let what = match eco {
            Ecosystem::Npm => "package.json",
            Ecosystem::Cargo => "Cargo.toml",
            Ecosystem::Python => "requirements*.txt or pyproject.toml",
            Ecosystem::Go => "go.mod",
        };
        Err(format!("no {what} found"))
    }
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

fn manifest_files(eco: Ecosystem, d: &Path) -> Vec<PathBuf> {
    match eco {
        Ecosystem::Npm => vec![d.join("package.json")],
        Ecosystem::Cargo => vec![d.join("Cargo.toml")],
        Ecosystem::Go => vec![d.join("go.mod")],
        Ecosystem::Python => {
            let mut v: Vec<PathBuf> = std::fs::read_dir(d)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    file_name(p).starts_with("requirements")
                        && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("txt"))
                })
                .collect();
            v.sort();
            v.push(d.join("pyproject.toml"));
            v
        }
    }
}

fn parse_manifest(eco: Ecosystem, f: &Path, text: &str, names: &mut HashSet<String>) -> Result<(), String> {
    let is_pyproject = file_name(f) == "pyproject.toml";
    match eco {
        Ecosystem::Npm => {
            let v: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
            for sec in [
                "dependencies",
                "devDependencies",
                "optionalDependencies",
                "peerDependencies",
            ] {
                if let Some(o) = v.get(sec).and_then(serde_json::Value::as_object) {
                    names.extend(o.keys().cloned());
                }
            }
        }
        Ecosystem::Cargo => {
            let t: toml::Table = text
                .parse()
                .map_err(|e: toml::de::Error| e.message().to_string())?;
            let ws = t.get("workspace").and_then(toml::Value::as_table);
            for sec in [
                t.get("dependencies"),
                t.get("dev-dependencies"),
                t.get("build-dependencies"),
                ws.and_then(|w| w.get("dependencies")),
            ] {
                if let Some(tbl) = sec.and_then(toml::Value::as_table) {
                    names.extend(tbl.keys().cloned());
                }
            }
        }
        Ecosystem::Python if is_pyproject => {
            let t: toml::Table = text
                .parse()
                .map_err(|e: toml::de::Error| e.message().to_string())?;
            let project = t.get("project").and_then(toml::Value::as_table);
            let groups = [
                project.and_then(|p| p.get("optional-dependencies")),
                t.get("dependency-groups"),
            ]
            .into_iter()
            .flatten()
            .filter_map(toml::Value::as_table)
            .flat_map(|tbl| tbl.values());
            let reqs = project
                .and_then(|p| p.get("dependencies"))
                .into_iter()
                .chain(groups)
                .filter_map(toml::Value::as_array)
                .flatten()
                .filter_map(toml::Value::as_str);
            names.extend(reqs.map(pep503).filter(|n| !n.is_empty()));
        }
        // ponytail: line-anchored requirement names (no PEP 508 parser in the
        // workspace); an `-e`/URL-only line declares nothing here.
        Ecosystem::Python => {
            let lines = text.lines().map(str::trim).filter(|l| !l.starts_with(['#', '-']));
            names.extend(lines.map(pep503).filter(|n| !n.is_empty()));
        }
        // ponytail: line-anchored `require` parsing (no go.mod parser in the
        // workspace); `replace`/`exclude` directives are not read.
        Ecosystem::Go => {
            let mut block = false;
            for l in text.lines().map(str::trim) {
                let module = if block {
                    block = !l.starts_with(')');
                    l
                } else if let Some(r) = l.strip_prefix("require ").map(str::trim) {
                    block = r.starts_with('(');
                    r
                } else {
                    continue;
                };
                if let Some(m) = module
                    .split_whitespace()
                    .next()
                    .filter(|m| !m.starts_with(['(', ')', '/']))
                {
                    names.insert(m.to_string());
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::reason;
    use std::path::Path;

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let d = tempfile::tempdir().expect("tempdir");
        for (name, body) in files {
            let p = d.path().join(name);
            std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            std::fs::write(p, body).expect("write");
        }
        d
    }

    fn gate(root: &Path, cmd: &str) -> Option<String> {
        reason(cmd, None, root)
    }

    #[test]
    fn npm_install_of_an_undeclared_package_needs_approval() {
        let d = project(&[(
            "package.json",
            r#"{"dependencies":{"react":"^18"},"devDependencies":{"@types/node":"1"}}"#,
        )]);
        let r = gate(d.path(), "npm install left-pad").expect("undeclared ⇒ approval");
        assert!(r.contains("left-pad"), "{r}");
        assert_eq!(gate(d.path(), "npm install"), None, "no package named");
        assert_eq!(gate(d.path(), "npm ci"), None);
        assert_eq!(
            gate(d.path(), "sudo npm install react@18.3.1"),
            None,
            "declared, version stripped"
        );
        assert_eq!(
            gate(d.path(), "pnpm add -D @types/node@^20"),
            None,
            "scoped, version stripped"
        );
        assert!(gate(d.path(), "npm i -g react").is_some(), "global ⇒ approval");
        for src in [
            "git+https://x/y.git",
            "user/repo",
            "./local",
            "../pkg.tgz",
            "https://x/a.tgz",
        ] {
            assert!(gate(d.path(), &format!("npm install {src}")).is_some(), "{src}");
        }
    }

    #[test]
    fn pip_requirements_are_matched_pep503_through_a_chain() {
        let d = project(&[(
            "requirements-dev.txt",
            "# dev\nRequests_OAuthlib==1.3\n-r base.txt\n",
        )]);
        let cmd = "cd app && pip install requests==2.31";
        let r = gate(d.path(), cmd).expect("requests absent ⇒ approval");
        assert!(r.contains("requests"), "{r}");
        assert_eq!(gate(d.path(), "pip install 'requests-oauthlib[socks]>=1'"), None);
        std::fs::write(d.path().join("requirements.txt"), "requests>=2\n").expect("write");
        assert_eq!(gate(d.path(), cmd), None, "requests present");
        assert_eq!(gate(d.path(), "pip install -r requirements.txt"), None);
        assert_eq!(gate(d.path(), "pip install -e ."), None);
        assert!(
            gate(d.path(), "pip install -e ../other").is_some(),
            "path ⇒ approval"
        );
    }

    #[test]
    fn pyproject_and_go_mod_declare_packages() {
        let d = project(&[
            (
                "pyproject.toml",
                "[project]\ndependencies = [\"Flask.Login>=1\"]\n[dependency-groups]\ndev = [\"pytest\"]\n",
            ),
            ("go.mod", "module m\n\nrequire example.com/a v1.0.0\nrequire (\n\texample.com/b v1.2.0 // indirect\n)\n"),
        ]);
        assert_eq!(gate(d.path(), "uv pip install flask_login pytest"), None);
        assert_eq!(gate(d.path(), "go get example.com/b@v1.3.0 example.com/a"), None);
        assert!(gate(d.path(), "go get example.com/c@v1.2.0").is_some());
        assert_eq!(gate(d.path(), "go get ./..."), None);
        assert_eq!(gate(d.path(), "go get"), None);
    }

    #[test]
    fn cargo_add_checks_every_dependency_table_and_fails_closed() {
        let d = project(&[
            (
                "Cargo.toml",
                "[workspace.dependencies]\ntokio = \"1\"\n[dependencies.serde]\nversion = \"1\"\n",
            ),
            ("crates/x/Cargo.toml", "[dev-dependencies]\nproptest = \"1\"\n"),
        ]);
        let member = Some("crates/x");
        for declared in [
            "cargo add serde@1",
            "bash -c \"cargo add tokio\"",
            "cargo add --dev proptest",
        ] {
            assert_eq!(reason(declared, member, d.path()), None, "{declared}");
        }
        let r = reason("bash -c \"cargo add anyhow\"", member, d.path()).expect("absent");
        assert!(r.contains("anyhow"), "{r}");
        assert!(reason("cargo add --git https://x/y y", member, d.path()).is_some());

        std::fs::write(d.path().join("Cargo.toml"), "[dependencies\nserde = ").expect("write");
        let r = gate(d.path(), "cargo add serde").expect("unparsable manifest ⇒ fail closed");
        assert!(r.contains("cannot parse Cargo.toml"), "{r}");
        let empty = project(&[]);
        let r = gate(empty.path(), "cargo add serde").expect("missing manifest ⇒ fail closed");
        assert!(r.contains("no Cargo.toml"), "{r}");
        assert!(
            reason("cargo add serde", Some(".."), d.path()).is_some(),
            "cwd outside root"
        );
    }
}
