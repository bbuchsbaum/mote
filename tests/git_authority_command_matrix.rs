use mote::Store;
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::{TempDir, tempdir};

struct Fixture {
    _root: TempDir,
    repo: PathBuf,
    remote: PathBuf,
    store: Store,
}

impl Fixture {
    fn new() -> Self {
        let root = tempdir().unwrap();
        let remote = root.path().join("authority.git");
        git(
            root.path(),
            &["init", "--bare", "-q", remote.to_str().unwrap()],
        );
        let repo = root.path().join("client");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(
            &repo,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        let store = Store::init(&repo).unwrap();
        let fixture = Self {
            _root: root,
            repo,
            remote,
            store,
        };
        let output = fixture.run(&["--json", "authority", "init-git"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        fixture
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mote"));
        isolate(&mut command);
        command
            .current_dir(&self.repo)
            .env_remove("MOTE_ACTOR")
            .env_remove("MOTE_SESSION")
            .env_remove("MOTE_STORE")
            .arg("--store")
            .arg(self.store.root())
            .arg("--actor")
            .arg("alice")
            .args(args);
        command.output().unwrap()
    }

    fn run_with_store_env(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mote"));
        isolate(&mut command);
        command
            .current_dir(&self.repo)
            .env_remove("MOTE_ACTOR")
            .env_remove("MOTE_SESSION")
            .env("MOTE_STORE", self.store.root())
            .arg("--actor")
            .arg("alice")
            .args(args)
            .output()
            .unwrap()
    }

    fn remote_head(&self) -> String {
        let mut command = Command::new("git");
        isolate(&mut command);
        let output = command
            .arg("--git-dir")
            .arg(&self.remote)
            .args(["rev-parse", "refs/heads/mote-state"])
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().into()
    }

    fn local_ops(&self) -> Vec<String> {
        let mut names = fs::read_dir(self.store.ops_dir())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    fn assert_refusal_keeps_state(&self, output: Output, expected: &str) {
        assert_eq!(
            output.status.code(),
            Some(3),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected),
            "stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn git(repo: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    isolate(&mut command);
    let output = command.arg("-C").arg(repo).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn isolate(command: &mut Command) {
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") || name.to_string_lossy().starts_with("MOTE_")
        {
            command.env_remove(name);
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0");
}

#[test]
fn bound_authority_refuses_unsupported_variants_before_input_or_effects() {
    let fixture = Fixture::new();
    let before_head = fixture.remote_head();
    let before_ops = fixture.local_ops();
    let missing_batch = fixture.repo.join("missing.jsonl");
    let missing_import = fixture.repo.join("missing.json");

    for (args, expected) in [
        (
            vec!["new", "task", "--tag", "unsupported"],
            "--tag, --dep, and --id are refused",
        ),
        (
            vec!["ready", "--graph"],
            "remote ready --graph is unsupported",
        ),
        (
            vec!["preflight", "--candidate", "ca-example"],
            "remote candidate reservations are unsupported",
        ),
        (
            vec!["batch", missing_batch.to_str().unwrap()],
            "not implemented by the remote adapter",
        ),
        (
            vec!["import", missing_import.to_str().unwrap()],
            "not implemented by the remote adapter",
        ),
        (
            vec!["candidate", "show", "ca-example"],
            "not implemented by the remote adapter",
        ),
        (
            vec!["role", "list"],
            "not implemented by the remote adapter",
        ),
        (vec!["ui"], "not implemented by the remote adapter"),
        (
            vec!["serve", "--port", "0"],
            "not implemented by the remote adapter",
        ),
    ] {
        fixture.assert_refusal_keeps_state(fixture.run(&args), expected);
        assert_eq!(fixture.remote_head(), before_head, "args={args:?}");
        assert_eq!(fixture.local_ops(), before_ops, "args={args:?}");
    }
    assert!(!missing_batch.exists());
    assert!(!missing_import.exists());

    let forced = fixture.run_with_store_env(&["release", "bd-example", "--force"]);
    fixture.assert_refusal_keeps_state(forced, "--force is unsupported by Git authority release");
    assert_eq!(fixture.remote_head(), before_head);
    assert_eq!(fixture.local_ops(), before_ops);
}

#[test]
fn remote_session_as_normalizes_whitespace_through_store_actor_resolution() {
    let fixture = Fixture::new();
    let output = fixture.run(&["--json", "session", "start", "--as", "  alice  "]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["actor"], "alice");
    assert!(value["session_id"].as_str().unwrap().starts_with("sess-"));
    let mut command = Command::new(env!("CARGO_BIN_EXE_mote"));
    isolate(&mut command);
    let output = command
        .current_dir(&fixture.repo)
        .args(["--json", "--actor", "alice", "--store"])
        .arg(fixture.store.root())
        .args(["new", "usable trimmed session"])
        .env("MOTE_SESSION", value["session_id"].as_str().unwrap())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["status"],
        "accepted"
    );
}

#[test]
fn capabilities_are_storeless_and_hash_the_executed_binary() {
    let empty = tempdir().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_mote"));
    isolate(&mut command);
    let output = command
        .current_dir(empty.path())
        .env_remove("MOTE_STORE")
        .env_remove("MOTE_ACTOR")
        .env_remove("MOTE_SESSION")
        .args(["--json", "authority", "capabilities"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let expected = blake3::hash(&fs::read(env!("CARGO_BIN_EXE_mote")).unwrap())
        .to_hex()
        .to_string();
    assert_eq!(value["schema"], "mote.git-binary.v1");
    assert_eq!(value["executable"]["algorithm"], "blake3");
    assert_eq!(value["executable"]["digest"], expected);
    assert!(!empty.path().join(".mote").exists());
}

fn inventory(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, path: &Path, result: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                visit(root, &entry.path(), result);
            } else {
                result.insert(
                    entry.path().strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(entry.path()).unwrap(),
                );
            }
        }
    }
    let mut result = std::collections::BTreeMap::new();
    visit(root, root, &mut result);
    result
}

#[test]
fn inherited_selectors_do_not_touch_an_unrelated_repository() {
    let unrelated = tempdir().unwrap();
    git(unrelated.path(), &["init", "-q"]);
    fs::write(
        unrelated.path().join("owned.txt"),
        b"unrelated dirty work\n",
    )
    .unwrap();
    Store::init(unrelated.path()).unwrap();
    let before = inventory(unrelated.path());
    let mut command = Command::new(std::env::current_exe().unwrap());
    isolate(&mut command);
    let output = command
        .args([
            "--ignored",
            "--exact",
            "matrix_with_inherited_selectors_helper",
            "--nocapture",
        ])
        .env("GIT_DIR", unrelated.path().join(".git"))
        .env("GIT_WORK_TREE", unrelated.path())
        .env("GIT_COMMON_DIR", unrelated.path().join(".git"))
        .env(
            "GIT_OBJECT_DIRECTORY",
            unrelated.path().join(".git/objects"),
        )
        .env("GIT_INDEX_FILE", unrelated.path().join("unrelated-index"))
        .env("MOTE_STORE", unrelated.path().join(".mote"))
        .env("MOTE_ACTOR", "unrelated-actor")
        .env("MOTE_SESSION", "unrelated-session")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    assert_eq!(inventory(unrelated.path()), before);
}

#[test]
#[ignore = "owned subprocess helper for inherited-selector isolation"]
fn matrix_with_inherited_selectors_helper() {
    assert!(std::env::var_os("GIT_DIR").is_some());
    bound_authority_refuses_unsupported_variants_before_input_or_effects();
    remote_session_as_normalizes_whitespace_through_store_actor_resolution();
    capabilities_are_storeless_and_hash_the_executed_binary();
}
