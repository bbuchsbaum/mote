//! Store discovery from linked Git worktrees must not split coordination onto a
//! worktree's tracked copy of `.mote/`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

fn mote_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mote")
}

fn mote(dir: &Path, args: &[&str]) -> Output {
    Command::new(mote_bin())
        .args(args)
        .current_dir(dir)
        .env_remove("MOTE_ACTOR")
        .env_remove("MOTE_STORE")
        .env_remove("MOTE_SESSION")
        .output()
        .unwrap()
}

fn ok(out: &Output) -> String {
    assert!(
        out.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repo whose `.mote/` (one bead) is committed, plus a linked worktree that
/// therefore carries its own copy. Returns (tempdir, main, worktree, bead id).
fn repo_with_tracked_store() -> (TempDir, PathBuf, PathBuf, String) {
    let td = TempDir::new().unwrap();
    let main = td.path().join("main");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    ok(&mote(&main, &["init"]));
    let id = ok(&mote(&main, &["new", "shared work", "--actor", "alice"]))
        .trim()
        .to_string();
    git(&main, &["add", "-f", ".mote/FORMAT.json", ".mote/ops"]);
    git(&main, &["commit", "-q", "-m", "track store"]);
    let wt = td.path().join("wt");
    git(
        &main,
        &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()],
    );
    assert!(
        wt.join(".mote/FORMAT.json").is_file(),
        "worktree has a copy"
    );
    // Git does not carry the empty scratch directories; a pre-fix client run
    // in the worktree would have recreated them.
    for dir in ["tmp", "local"] {
        std::fs::create_dir_all(wt.join(".mote").join(dir)).unwrap();
    }
    (td, main, wt, id)
}

#[test]
fn tracked_copy_in_linked_worktree_resolves_to_shared_store() {
    let (_td, main, wt, id) = repo_with_tracked_store();

    ok(&mote(&wt, &["claim", &id, "--actor", "a"]));
    let competing = mote(&main, &["claim", &id, "--actor", "b"]);
    assert_eq!(
        competing.status.code(),
        Some(2),
        "the worktree claim must be visible from the main checkout: {}",
        String::from_utf8_lossy(&competing.stderr)
    );

    // Nothing was written to the worktree's private copy.
    let copy_ops = std::fs::read_dir(wt.join(".mote/ops")).unwrap().count();
    let shared_ops = std::fs::read_dir(main.join(".mote/ops")).unwrap().count();
    assert!(
        copy_ops < shared_ops,
        "copy {copy_ops} vs shared {shared_ops}"
    );

    let doctor: serde_json::Value =
        serde_json::from_str(&ok(&mote(&wt, &["--json", "doctor", "--actor", "a"]))).unwrap();
    let store_root = PathBuf::from(doctor["store_root"].as_str().unwrap());
    assert_eq!(
        std::fs::canonicalize(store_root).unwrap(),
        std::fs::canonicalize(main.join(".mote")).unwrap()
    );
    assert_eq!(doctor["worktree"]["redirected"], true);
    assert_eq!(doctor["worktree"]["copy_only_ops"], 0);
    let text = ok(&mote(&wt, &["doctor", "--actor", "a"]));
    assert!(text.contains("worktree: linked"), "{text}");
}

#[test]
fn explicit_store_flag_and_env_are_still_honored_in_a_worktree() {
    let (_td, main, wt, id) = repo_with_tracked_store();
    let copy = wt.join(".mote");

    ok(&mote(
        &wt,
        &[
            "--store",
            copy.to_str().unwrap(),
            "claim",
            &id,
            "--actor",
            "a",
        ],
    ));
    // The main store never saw that claim.
    ok(&mote(&main, &["claim", &id, "--actor", "b"]));

    let env_claim = Command::new(mote_bin())
        .args(["claim", &id, "--actor", "c"])
        .current_dir(&main)
        .env("MOTE_STORE", &copy)
        .env_remove("MOTE_ACTOR")
        .output()
        .unwrap();
    assert_eq!(
        env_claim.status.code(),
        Some(2),
        "MOTE_STORE pointed at the copy, where a is the holder"
    );
}

#[test]
fn doctor_reports_ops_stranded_in_a_diverged_worktree_copy() {
    let (_td, _main, wt, id) = repo_with_tracked_store();
    // Simulate a pre-fix client that wrote into the copy directly.
    let copy = wt.join(".mote");
    ok(&mote(
        &wt,
        &[
            "--store",
            copy.to_str().unwrap(),
            "note",
            &id,
            "--kind",
            "progress",
            "lost note",
            "--actor",
            "a",
        ],
    ));

    let doctor: serde_json::Value =
        serde_json::from_str(&ok(&mote(&wt, &["--json", "doctor", "--actor", "a"]))).unwrap();
    assert_eq!(doctor["worktree"]["copy_only_ops"], 1);
    let warnings = doctor["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| w
            .as_str()
            .unwrap()
            .contains("missing from the shared store")),
        "{warnings:?}"
    );
}

#[test]
fn worktree_store_with_a_different_identity_is_used_as_found() {
    let td = TempDir::new().unwrap();
    let main = td.path().join("main");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    ok(&mote(&main, &["init"]));
    std::fs::write(main.join("README"), "x").unwrap();
    git(&main, &["add", "README"]);
    git(&main, &["commit", "-q", "-m", "init"]);
    let wt = td.path().join("wt");
    git(
        &main,
        &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()],
    );
    // A plain init here reports the shared store instead of forking it.
    let plain = ok(&mote(&wt, &["init"]));
    assert!(plain.contains("main worktree's store"), "{plain}");
    assert!(!wt.join(".mote").exists());
    ok(&mote(&wt, &["init", "--separate"]));
    let id = ok(&mote(&wt, &["new", "private", "--actor", "a"]))
        .trim()
        .to_string();
    let show = mote(&main, &["show", &id]);
    assert!(!show.status.success(), "a separate store stays separate");
    let doctor: serde_json::Value =
        serde_json::from_str(&ok(&mote(&wt, &["--json", "doctor", "--actor", "a"]))).unwrap();
    assert!(doctor["worktree"].is_null());
}

#[test]
fn worktree_without_its_own_store_falls_back_to_the_main_store() {
    let td = TempDir::new().unwrap();
    let main = td.path().join("main");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    ok(&mote(&main, &["init"]));
    let id = ok(&mote(
        &main,
        &["new", "untracked store", "--actor", "alice"],
    ))
    .trim()
    .to_string();
    std::fs::write(main.join("README"), "x").unwrap();
    git(&main, &["add", "README"]);
    git(&main, &["commit", "-q", "-m", "init"]);
    let wt = td.path().join("wt");
    git(
        &main,
        &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()],
    );
    assert!(!wt.join(".mote").exists());

    ok(&mote(&wt, &["claim", &id, "--actor", "a"]));
    assert_eq!(
        mote(&main, &["claim", &id, "--actor", "b"]).status.code(),
        Some(2)
    );
}

#[test]
fn actor_identity_stays_with_the_checkout_under_a_redirect() {
    let (_td, main, wt, _id) = repo_with_tracked_store();
    ok(&mote(&main, &["actor", "set", "main-agent"]));
    ok(&mote(&wt, &["actor", "set", "wt-agent"]));

    assert!(ok(&mote(&main, &["actor", "show"])).contains("main-agent"));
    assert!(ok(&mote(&wt, &["actor", "show"])).contains("wt-agent"));
    assert_eq!(
        std::fs::read_to_string(main.join(".mote/local/actor"))
            .unwrap()
            .trim(),
        "main-agent",
        "setting the worktree identity must not clobber the main checkout's"
    );
}

#[test]
fn actor_identity_for_a_storeless_worktree_lives_in_its_git_admin_dir() {
    let td = TempDir::new().unwrap();
    let main = td.path().join("main");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    ok(&mote(&main, &["init"]));
    std::fs::write(main.join("README"), "x").unwrap();
    git(&main, &["add", "README"]);
    git(&main, &["commit", "-q", "-m", "init"]);
    let wt = td.path().join("wt");
    git(
        &main,
        &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()],
    );
    ok(&mote(&main, &["actor", "set", "main-agent"]));
    ok(&mote(&wt, &["actor", "set", "wt-agent"]));
    assert!(ok(&mote(&main, &["actor", "show"])).contains("main-agent"));
    assert!(ok(&mote(&wt, &["actor", "show"])).contains("wt-agent"));
    assert!(
        !wt.join(".mote").exists(),
        "no untracked store dir appears in the worktree"
    );
}

#[test]
fn worktree_nested_in_the_main_checkout_keeps_its_own_identity() {
    let td = TempDir::new().unwrap();
    let main = td.path().join("main");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    ok(&mote(&main, &["init"]));
    let id = ok(&mote(&main, &["new", "nested", "--actor", "alice"]))
        .trim()
        .to_string();
    std::fs::write(main.join(".gitignore"), ".mote/\n.claude/\n").unwrap();
    git(&main, &["add", ".gitignore"]);
    git(&main, &["commit", "-q", "-m", "init"]);
    let wt = main.join(".claude/worktrees/x");
    git(
        &main,
        &["worktree", "add", "-q", "-b", "x", wt.to_str().unwrap()],
    );
    assert!(!wt.join(".mote").exists());

    ok(&mote(&main, &["actor", "set", "main-agent"]));
    ok(&mote(&wt, &["actor", "set", "wt-agent"]));
    assert!(ok(&mote(&main, &["actor", "show"])).contains("main-agent"));
    assert!(ok(&mote(&wt, &["actor", "show"])).contains("wt-agent"));

    // Coordination is still shared through the main store.
    ok(&mote(&wt, &["claim", &id]));
    let competing = mote(&main, &["claim", &id]);
    assert_eq!(competing.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&competing.stderr).contains("wt-agent"));

    let text = ok(&mote(&wt, &["doctor"]));
    assert!(text.contains("worktree: linked"), "{text}");
}

fn nested_worktree() -> (TempDir, PathBuf, PathBuf) {
    let td = TempDir::new().unwrap();
    let main = td.path().join("main");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    ok(&mote(&main, &["init"]));
    std::fs::write(main.join(".gitignore"), ".mote/\n.claude/\n").unwrap();
    git(&main, &["add", ".gitignore"]);
    git(&main, &["commit", "-q", "-m", "init"]);
    let wt = main.join(".claude/worktrees/x");
    git(
        &main,
        &["worktree", "add", "-q", "-b", "x", wt.to_str().unwrap()],
    );
    (td, main, wt)
}

#[test]
fn explicit_mote_store_keeps_the_worktree_identity() {
    let (_td, main, wt) = nested_worktree();
    ok(&mote(&main, &["actor", "set", "main-agent"]));
    ok(&mote(&wt, &["actor", "set", "wt-agent"]));
    let shown = Command::new(mote_bin())
        .args(["actor", "show"])
        .current_dir(&wt)
        .env("MOTE_STORE", main.join(".mote"))
        .env_remove("MOTE_ACTOR")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&ok(&shown).into_bytes()).contains("wt-agent"));
}

#[test]
fn unresolved_worktree_identity_names_the_remedy() {
    let (_td, main, wt) = nested_worktree();
    ok(&mote(&main, &["actor", "set", "main-agent"]));
    let out = mote(&wt, &["actor", "show"]);
    assert_eq!(out.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("linked worktree keeps its own identity"),
        "{stderr}"
    );
    assert!(stderr.contains("mote actor set"), "{stderr}");
}

#[test]
fn init_in_a_sibling_worktree_does_not_fork_the_store() {
    let td = TempDir::new().unwrap();
    let main = td.path().join("main");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    ok(&mote(&main, &["init"]));
    let id = ok(&mote(&main, &["new", "shared", "--actor", "alice"]))
        .trim()
        .to_string();
    std::fs::write(main.join("README"), "x").unwrap();
    git(&main, &["add", "README"]);
    git(&main, &["commit", "-q", "-m", "init"]);
    let sib = td.path().join("sib");
    git(
        &main,
        &["worktree", "add", "-q", "-b", "sib", sib.to_str().unwrap()],
    );
    ok(&mote(&sib, &["init"]));
    assert!(!sib.join(".mote").exists());
    ok(&mote(&sib, &["show", &id]));
}
