use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

use mote::git_authority::transport::{GitTransport, PushFailure};
use tempfile::TempDir;

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn fixture() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let root = TempDir::new().unwrap();
    let bare = root.path().join("authority.git");
    let repo = root.path().join("source");
    git(root.path(), &["init", "--bare", bare.to_str().unwrap()]);
    git(root.path(), &["init", repo.to_str().unwrap()]);
    git(&repo, &["config", "user.name", "Test"]);
    git(&repo, &["config", "user.email", "test@example.invalid"]);
    (root, repo, bare)
}

#[cfg(unix)]
struct HangingHelperGuard {
    pid_file: PathBuf,
}

#[cfg(unix)]
impl Drop for HangingHelperGuard {
    fn drop(&mut self) {
        let Ok(pid) = fs::read_to_string(&self.pid_file) else {
            return;
        };
        let pid = pid.trim();
        if pid.bytes().all(|byte| byte.is_ascii_digit()) && !pid.is_empty() {
            let _ = Command::new("kill")
                .args(["-TERM", "--", pid])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
}

#[cfg(unix)]
fn pid_or_group_exists(pid: &str, group: bool) -> bool {
    let target = if group { format!("-{pid}") } else { pid.into() };
    Command::new("kill")
        .args(["-0", "--", &target])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[test]
fn creates_reads_and_pushes_a_cumulative_tree_without_touching_source_state() {
    let (root, repo, bare) = fixture();
    git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
    fs::write(repo.join("source.txt"), b"tracked\n").unwrap();
    git(&repo, &["add", "source.txt"]);
    git(&repo, &["commit", "-m", "source"]);
    git(&repo, &["push", "origin", "HEAD:refs/heads/source"]);
    git(&repo, &["fetch", "origin", "refs/heads/source"]);
    fs::write(repo.join("source.txt"), b"dirty\n").unwrap();
    let refs = git(&repo, &["show-ref"]);
    let index = fs::read(repo.join(".git/index")).unwrap();
    let fetch_head = fs::read(repo.join(".git/FETCH_HEAD")).unwrap();
    let status = git(&repo, &["status", "--porcelain"]);
    let hook = repo.join(".git/hooks/pre-push");
    fs::write(
        &hook,
        "#!/bin/sh\n[ \"$(git config mote.transport-sentinel)\" = present ]\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(&repo, &["config", "mote.transport-sentinel", "present"]);
    git(
        &repo,
        &["config", "remote.origin.uploadpack", "git-upload-pack"],
    );
    git(
        &repo,
        &["config", "remote.origin.receivepack", "git-receive-pack"],
    );

    let transport = GitTransport::open(
        &repo,
        "origin",
        "mote-state",
        &root.path().join("scratch-a"),
    )
    .unwrap();
    assert_eq!(transport.head().unwrap(), None);
    assert_eq!(transport.branch_ref(), "refs/heads/mote-state");
    assert_eq!(transport.endpoint_digest().len(), 64);
    let one = BTreeMap::from([("manifest.json".into(), b"one\0raw".to_vec())]);
    let first = transport.create_commit(None, &one).unwrap();
    assert!(transport.push(&first).unwrap().success);
    assert_eq!(transport.head().unwrap().as_deref(), Some(first.as_str()));
    let two = BTreeMap::from([(
        "transactions/00000000000000000001.json".into(),
        b"two".to_vec(),
    )]);
    let second = transport.create_commit(Some(&first), &two).unwrap();
    assert!(transport.push(&second).unwrap().success);
    let commit = transport.commit(&second).unwrap();
    assert_eq!(commit.parents, vec![first]);
    assert_eq!(commit.files["manifest.json"].bytes, b"one\0raw");
    assert_eq!(
        commit.files["transactions/00000000000000000001.json"].bytes,
        b"two"
    );
    assert_eq!(
        (
            git(&repo, &["show-ref"]),
            fs::read(repo.join(".git/index")).unwrap(),
            fs::read(repo.join(".git/FETCH_HEAD")).unwrap(),
            git(&repo, &["status", "--porcelain"])
        ),
        (refs, index, fetch_head, status)
    );
}

#[test]
fn uses_the_single_push_endpoint_and_reports_contention_and_denial() {
    let (root, repo, bare) = fixture();
    git(&repo, &["remote", "add", "origin", "/does/not/exist"]);
    git(
        &repo,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            bare.to_str().unwrap(),
        ],
    );
    let a = GitTransport::open(
        &repo,
        "origin",
        "mote-state",
        &root.path().join("scratch-a"),
    )
    .unwrap();
    let b = GitTransport::open(
        &repo,
        "origin",
        "mote-state",
        &root.path().join("scratch-b"),
    )
    .unwrap();
    let root_commit = a
        .create_commit(
            None,
            &BTreeMap::from([("manifest.json".into(), b"root".to_vec())]),
        )
        .unwrap();
    assert!(a.push(&root_commit).unwrap().success);
    let head = b.head().unwrap().unwrap();
    let left = a
        .create_commit(
            Some(&root_commit),
            &BTreeMap::from([(
                "transactions/00000000000000000001.json".into(),
                b"a".to_vec(),
            )]),
        )
        .unwrap();
    let right = b
        .create_commit(
            Some(&head),
            &BTreeMap::from([(
                "transactions/00000000000000000001.json".into(),
                b"b".to_vec(),
            )]),
        )
        .unwrap();
    let attempts = thread::scope(|scope| {
        let one = scope.spawn(|| a.push(&left).unwrap());
        let two = scope.spawn(|| b.push(&right).unwrap());
        [one.join().unwrap(), two.join().unwrap()]
    });
    assert_eq!(attempts.iter().filter(|attempt| attempt.success).count(), 1);
    assert!(
        attempts
            .iter()
            .any(|attempt| attempt.failure == Some(PushFailure::Contention))
    );
    fs::write(bare.join("hooks/pre-receive"), "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            bare.join("hooks/pre-receive"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let current = a.head().unwrap().unwrap();
    let denied = a
        .create_commit(
            Some(&current),
            &BTreeMap::from([(
                "transactions/00000000000000000002.json".into(),
                b"denied".to_vec(),
            )]),
        )
        .unwrap();
    let attempt = a.push(&denied).unwrap();
    assert!(!attempt.success);
    assert!(attempt.exit_code.is_some());
    assert_eq!(attempt.failure, Some(PushFailure::Denied));
}

#[test]
fn rejects_bad_remote_branch_and_object_identifiers() {
    let (root, repo, bare) = fixture();
    git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
    assert!(GitTransport::open(&repo, "--upload-pack=x", "mote-state", root.path()).is_err());
    assert!(GitTransport::open(&repo, "origin", "bad..branch", root.path()).is_err());
    assert!(GitTransport::open(&repo, "origin", "mote-state", &repo).is_err());
    git(
        &repo,
        &[
            "remote",
            "set-url",
            "--add",
            "--push",
            "origin",
            "/one/endpoint",
        ],
    );
    git(
        &repo,
        &[
            "remote",
            "set-url",
            "--add",
            "--push",
            "origin",
            "/another/endpoint",
        ],
    );
    assert!(
        GitTransport::open(
            &repo,
            "origin",
            "mote-state",
            &root.path().join("multiple-pushurls")
        )
        .is_err()
    );
    git(
        &repo,
        &[
            "remote",
            "set-url",
            "--delete",
            "--push",
            "origin",
            "/another/endpoint",
        ],
    );
    git(
        &repo,
        &[
            "remote",
            "set-url",
            "--delete",
            "--push",
            "origin",
            "/one/endpoint",
        ],
    );
    git(&repo, &["config", "remote.origin.mirror", "true"]);
    assert!(
        GitTransport::open(
            &repo,
            "origin",
            "mote-state",
            &root.path().join("unsupported-setting")
        )
        .is_err()
    );
    git(&repo, &["config", "--unset", "remote.origin.mirror"]);
    let transport =
        GitTransport::open(&repo, "origin", "mote-state", &root.path().join("scratch")).unwrap();
    assert!(transport.commit("deadbeef").is_err());
    assert!(transport.push(&"A".repeat(40)).is_err());
}

#[test]
#[cfg(unix)]
fn stderr_only_access_and_network_failures_are_classified_without_exposing_diagnostics() {
    use std::os::unix::fs::PermissionsExt;
    let (root, repo, bare) = fixture();
    git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
    let helper = root.path().join("refusing-receive-pack");
    git(
        &repo,
        &[
            "config",
            "remote.origin.receivepack",
            helper.to_str().unwrap(),
        ],
    );
    let scratch = root.path().join("scratch");
    let transport = GitTransport::open(&repo, "origin", "mote-state", &scratch).unwrap();
    let oid = transport
        .create_commit(
            None,
            &BTreeMap::from([("manifest.json".into(), b"root".to_vec())]),
        )
        .unwrap();
    for (message, expected) in [
        (
            "Permission denied fixture-credential-marker",
            PushFailure::Denied,
        ),
        (
            "Could not resolve host: fixture-credential-marker",
            PushFailure::Unavailable,
        ),
        (
            "Connection timed out fixture-credential-marker",
            PushFailure::Timeout,
        ),
        (
            "Unrecognized fixture-credential-marker",
            PushFailure::Unknown,
        ),
    ] {
        fs::write(
            &helper,
            format!("#!/bin/sh\nprintf '%s\\n' '{message}' >&2\nexit 1\n"),
        )
        .unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
        let attempt = transport.push(&oid).unwrap();
        assert!(!attempt.success);
        assert_eq!(attempt.failure, Some(expected));
        assert!(!format!("{attempt:?}").contains("fixture-credential-marker"));
        assert!(fs::read_dir(&scratch).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("git-")
        }));
    }
    assert!(transport.head().unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn timeout_terminates_the_owned_receive_helper_and_cleans_scratch() {
    let (root, repo, bare) = fixture();
    git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
    fs::write(repo.join("source.txt"), b"tracked\n").unwrap();
    git(&repo, &["add", "source.txt"]);
    git(&repo, &["commit", "-m", "source"]);
    git(&repo, &["push", "origin", "HEAD:refs/heads/source"]);
    git(&repo, &["fetch", "origin", "refs/heads/source"]);
    fs::write(repo.join("source.txt"), b"dirty\n").unwrap();
    let source_state = (
        git(&repo, &["show-ref"]),
        fs::read(repo.join(".git/index")).unwrap(),
        fs::read(repo.join(".git/FETCH_HEAD")).unwrap(),
        git(&repo, &["status", "--porcelain"]),
    );

    let bootstrap = GitTransport::open(
        &repo,
        "origin",
        "mote-state",
        &root.path().join("bootstrap-scratch"),
    )
    .unwrap();
    let root_commit = bootstrap
        .create_commit(
            None,
            &BTreeMap::from([("manifest.json".into(), b"root".to_vec())]),
        )
        .unwrap();
    assert!(bootstrap.push(&root_commit).unwrap().success);

    let pid_file = root.path().join("owned-receive-pack.pid");
    let helper = root.path().join("owned-hanging-receive-pack.sh");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\ntrap 'exit 0' TERM INT\nwhile :; do sleep 1; done\n",
            pid_file.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &repo,
        &[
            "config",
            "remote.origin.receivepack",
            helper.to_str().unwrap(),
        ],
    );
    let scratch = root.path().join("timeout-scratch");
    let transport = GitTransport::open(&repo, "origin", "mote-state", &scratch)
        .unwrap()
        .with_timeout(Duration::from_secs(5));
    let parent = transport.head().unwrap().unwrap();
    let proposal = transport
        .create_commit(
            Some(&parent),
            &BTreeMap::from([(
                "transactions/00000000000000000001.json".into(),
                b"timeout".to_vec(),
            )]),
        )
        .unwrap();
    let _guard = HangingHelperGuard {
        pid_file: pid_file.clone(),
    };
    let started = Instant::now();
    let attempt = transport.push(&proposal).unwrap();
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(!attempt.success);
    assert_eq!(attempt.exit_code, None);
    assert_eq!(attempt.failure, Some(PushFailure::Timeout));
    let pid = fs::read_to_string(&pid_file)
        .expect("owned helper must have started before the timeout to qualify termination");
    let pid = pid.trim();
    assert!(!pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()));
    let deadline = Instant::now() + Duration::from_secs(2);
    while (pid_or_group_exists(pid, false) || pid_or_group_exists(pid, true))
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!pid_or_group_exists(pid, false));
    assert!(!pid_or_group_exists(pid, true));
    assert!(fs::read_dir(&scratch).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".stderr")
    }));
    assert_eq!(
        (
            git(&repo, &["show-ref"]),
            fs::read(repo.join(".git/index")).unwrap(),
            fs::read(repo.join(".git/FETCH_HEAD")).unwrap(),
            git(&repo, &["status", "--porcelain"]),
        ),
        source_state
    );
}
