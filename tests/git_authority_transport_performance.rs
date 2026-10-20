use mote::git_authority::transport::GitTransport;
use std::{fs, path::Path, process::Command};
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

#[test]
fn linked_worktree_uses_common_objects_and_replacement_refs_do_not_reinterpret_authority() {
    let root = TempDir::new().unwrap();
    let repo = root.path().join("repo");
    let worktree = root.path().join("linked");
    let bare = root.path().join("remote.git");
    git(root.path(), &["init", repo.to_str().unwrap()]);
    git(root.path(), &["init", "--bare", bare.to_str().unwrap()]);
    git(&repo, &["config", "user.name", "t"]);
    git(&repo, &["config", "user.email", "t@e"]);
    fs::write(repo.join("seed"), b"one").unwrap();
    git(&repo, &["add", "seed"]);
    git(&repo, &["commit", "-m", "one"]);
    let original = git(&repo, &["rev-parse", "HEAD"]);
    fs::write(repo.join("seed"), b"replacement").unwrap();
    git(&repo, &["commit", "-am", "replacement"]);
    let replacement = git(&repo, &["rev-parse", "HEAD"]);
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "linked",
            worktree.to_str().unwrap(),
        ],
    );
    git(
        &worktree,
        &["remote", "add", "origin", bare.to_str().unwrap()],
    );
    let transport = GitTransport::open(
        &worktree,
        "origin",
        "authority",
        &root.path().join("scratch"),
    )
    .unwrap();
    git(
        &repo,
        &[
            "update-ref",
            &format!("refs/replace/{original}"),
            &replacement,
        ],
    );
    assert_eq!(
        transport.commit(&original).unwrap().files["seed"].bytes,
        b"one"
    );
}
