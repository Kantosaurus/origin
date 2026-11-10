// SPDX-License-Identifier: Apache-2.0
//! C8: against real git, a shadow repo at `<root>/.origin/shadow.git` never
//! records itself, a restore never writes into it (also from a legacy
//! checkpoint that recorded it), and the user's global git config (hooks,
//! signing) neither breaks nor runs inside shadow commands.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use origin_vcs::{GitRunner, RestoreMode, ShadowGit, VcsError};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Runs git in `root` with `GIT_CONFIG_GLOBAL` set to a hostile config: commit
/// signing with a gpg that does not exist, and a hooks dir whose `pre-commit`
/// fails and whose ref/checkout hooks leave a marker file.
struct Git {
    root: PathBuf,
    global: PathBuf,
}

impl GitRunner for Git {
    fn run(&self, args: &[&str]) -> Result<String, VcsError> {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .env("GIT_CONFIG_GLOBAL", &self.global)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .map_err(|e| VcsError::Git(e.to_string()))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(VcsError::Git(String::from_utf8_lossy(&out.stderr).into_owned()))
        }
    }
}

struct Repo {
    dir: PathBuf,
    git: Git,
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Repo {
    /// A work tree at `<tmp>/root` whose shadow is initialised the way
    /// `origin checkpoint` (origin-cli vcs.rs) does it.
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("origin-vcs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("root");
        let hooks = dir.join("user-hooks");
        std::fs::create_dir_all(root.join(".origin")).unwrap();
        std::fs::create_dir_all(&hooks).unwrap();
        let marker = dir.join("hook-ran").to_string_lossy().replace('\\', "/");
        let touch = format!("#!/bin/sh\necho ran >> '{marker}'\nexit 0\n");
        for (hook, body) in [
            ("pre-commit", "#!/bin/sh\nexit 1\n".to_string()),
            ("reference-transaction", touch.clone()),
            ("post-index-change", touch.clone()),
            ("post-checkout", touch),
        ] {
            let p = hooks.join(hook);
            std::fs::write(&p, body).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let global = dir.join("global.gitconfig");
        std::fs::write(
            &global,
            format!(
                "[commit]\n\tgpgsign = true\n[gpg]\n\tprogram = origin-no-such-gpg\n[core]\n\thooksPath = {}\n",
                hooks.to_string_lossy().replace('\\', "/")
            ),
        )
        .unwrap();
        let repo = Self {
            git: Git {
                root: root.clone(),
                global,
            },
            dir,
        };
        let root_s = root.to_string_lossy().into_owned();
        repo.raw(&["--work-tree", &root_s, "init", "-q"]);
        repo.raw(&["config", "core.worktree", &root_s]);
        repo
    }

    fn root(&self) -> &Path {
        &self.git.root
    }

    fn shadow(&self) -> ShadowGit<'_> {
        ShadowGit::new(&self.git, self.shadow_dir())
    }

    fn shadow_dir(&self) -> String {
        self.root()
            .join(".origin")
            .join("shadow.git")
            .to_string_lossy()
            .into_owned()
    }

    /// A git command against the shadow, outside the code under test (its
    /// commits carry their own identity and skip hooks/signing).
    fn raw(&self, args: &[&str]) -> String {
        let dir = self.shadow_dir();
        let hooks = format!("core.hooksPath={dir}/hooks");
        let mut full = vec![
            "-c",
            &hooks,
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "--git-dir",
            &dir,
        ];
        full.extend_from_slice(args);
        self.git
            .run(&full)
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"))
    }

    fn write(&self, rel: &str, body: &str) {
        let p = self.root().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.root().join(rel)).ok()
    }

    fn tree(&self, rev: &str) -> Vec<String> {
        self.raw(&["ls-tree", "-r", "--name-only", rev])
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn hook_ran(&self) -> bool {
        self.dir.join("hook-ran").exists()
    }

    /// The shadow still works: history readable, `last-green` resolves to
    /// `green`, and a further checkpoint succeeds without recording itself.
    fn assert_intact(&self, green: &str) {
        assert!(!self.shadow().list().unwrap().is_empty(), "log works");
        assert_eq!(self.raw(&["rev-parse", "refs/tags/last-green"]).trim(), green);
        let next = self.shadow().snapshot("after", 3).expect("a later checkpoint");
        assert!(!self
            .tree(&next.id)
            .iter()
            .any(|p| p.starts_with(".origin/shadow.git")));
    }
}

fn no_shadow_paths(files: &[String]) -> bool {
    !files.iter().any(|p| p.starts_with(".origin/shadow.git"))
}

#[test]
fn a_checkpoint_never_records_the_shadow_dir_and_runs_no_user_hook() {
    let r = Repo::new("record");
    r.write("a.txt", "A");
    r.write(".origin/config.toml", "user = 1");
    let cp = r.shadow().snapshot("one", 1).expect("checkpoint");
    let files = r.tree(&cp.id);
    assert!(files.contains(&"a.txt".to_string()), "{files:?}");
    assert!(
        files.contains(&".origin/config.toml".to_string()),
        "user config still recorded: {files:?}"
    );
    assert!(no_shadow_paths(&files), "{files:?}");
    assert!(!r.hook_ran(), "the user's hooks ran inside a shadow command");
}

#[test]
fn a_full_restore_matches_the_checkpoint_and_keeps_the_shadow_working() {
    let r = Repo::new("full");
    r.write("a.txt", "A");
    let a = r.shadow().snapshot("A", 1).expect("A").id;
    r.raw(&["update-ref", "refs/tags/last-green", &a]);
    r.write("a.txt", "B");
    r.write("b.txt", "B");
    r.shadow().snapshot("B", 2).expect("B");
    r.write("a.txt", "by hand");
    r.shadow().restore(&a, &RestoreMode::Full).expect("restore");
    assert_eq!(r.read("a.txt").as_deref(), Some("A"));
    assert_eq!(r.read("b.txt"), None, "a file A lacks is removed");
    assert_eq!(r.raw(&["rev-parse", "HEAD"]).trim(), a, "HEAD moved to A");
    assert!(!r.hook_ran(), "the user's hooks ran inside a shadow command");
    r.assert_intact(&a);
}

#[test]
fn a_full_restore_to_a_legacy_checkpoint_that_recorded_the_shadow_keeps_it_working() {
    let r = Repo::new("legacy");
    r.write("a.txt", "A");
    // The old self-tracking snapshot: `add -A` with nothing excluded.
    r.raw(&["add", "-A"]);
    r.raw(&["commit", "-q", "-m", "legacy A"]);
    let a = r.raw(&["rev-parse", "HEAD"]).trim().to_string();
    assert!(
        !no_shadow_paths(&r.tree(&a)),
        "precondition: A recorded the shadow dir"
    );
    r.raw(&["update-ref", "refs/tags/last-green", &a]);
    r.write("a.txt", "B");
    let b = r.shadow().snapshot("B", 2).expect("B");
    assert!(no_shadow_paths(&r.tree(&b.id)), "a new checkpoint untracks it");
    r.write("a.txt", "by hand");
    r.shadow().restore(&a, &RestoreMode::Full).expect("restore");
    assert_eq!(r.read("a.txt").as_deref(), Some("A"));
    r.assert_intact(&a);
}

/// One old self-tracking checkpoint: `add -A` with nothing excluded.
fn legacy_commit(r: &Repo, body: &str) -> String {
    r.write("a.txt", body);
    r.raw(&["add", "-A"]);
    r.raw(&["commit", "-q", "-m", body]);
    r.raw(&["rev-parse", "HEAD"]).trim().to_string()
}

#[test]
fn a_snapshot_after_a_full_rewind_of_a_two_commit_legacy_shadow_succeeds() {
    // The index still holds the newer commit's shadow entries while HEAD is
    // the older one: they match neither HEAD nor the files.
    let r = Repo::new("legacy-two");
    let a = legacy_commit(&r, "A");
    legacy_commit(&r, "B");
    r.shadow().restore(&a, &RestoreMode::Full).expect("restore");
    let next = r
        .shadow()
        .snapshot("after", 3)
        .expect("snapshot after the rewind");
    assert!(no_shadow_paths(&r.tree(&next.id)), "{:?}", r.tree(&next.id));
    assert_eq!(r.read("a.txt").as_deref(), Some("A"));
    assert!(!r.hook_ran(), "the user's hooks ran inside a shadow command");
}

#[test]
fn a_snapshot_after_a_failed_legacy_commit_succeeds() {
    // The old `add -A` ran, then its commit failed (signing, a hook): the
    // index holds shadow files that have changed on disk since.
    let r = Repo::new("legacy-failed");
    legacy_commit(&r, "A");
    r.write("a.txt", "B");
    r.raw(&["add", "-A"]);
    r.raw(&["update-ref", "refs/tags/moved", "HEAD"]);
    let next = r
        .shadow()
        .snapshot("after", 3)
        .expect("snapshot after the failed commit");
    assert!(no_shadow_paths(&r.tree(&next.id)), "{:?}", r.tree(&next.id));
}

#[test]
fn a_files_only_restore_still_works_and_keeps_head() {
    let r = Repo::new("files");
    r.write("a.txt", "A");
    let a = r.shadow().snapshot("A", 1).expect("A").id;
    r.write("a.txt", "B");
    let b = r.shadow().snapshot("B", 2).expect("B").id;
    r.raw(&["update-ref", "refs/tags/last-green", &a]);
    r.write("a.txt", "by hand");
    r.shadow()
        .restore(&a, &RestoreMode::WorkingTree)
        .expect("restore");
    assert_eq!(r.read("a.txt").as_deref(), Some("A"));
    assert_eq!(r.raw(&["rev-parse", "HEAD"]).trim(), b, "HEAD unmoved");
    assert!(!r.hook_ran(), "the user's hooks ran inside a shadow command");
    r.assert_intact(&a);
}
