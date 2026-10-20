use std::collections::BTreeMap;

use mote::git_authority::{
    connection::{RootArtifact, RootBlob},
    history::validate_chain,
    protocol::{MIGRATED_ROOT_CAPABILITIES, Manifest, encode},
    transport::{GitBlob, GitCommit},
};
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};
use tempfile::TempDir;

fn blob(bytes: Vec<u8>) -> GitBlob {
    GitBlob {
        mode: "100644".into(),
        oid: "b".repeat(40),
        bytes,
    }
}

fn migrated_root(manifest: Manifest, pack: Vec<u8>) -> GitCommit {
    GitCommit {
        oid: "1".repeat(40),
        parents: vec![],
        files: BTreeMap::from([
            ("manifest.json".into(), blob(encode(&manifest).unwrap())),
            ("legacy.pack".into(), blob(pack)),
        ]),
    }
}

fn git(dir: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = Command::new("git")
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    output.stdout
}

fn text(dir: &Path, args: &[&str]) -> String {
    String::from_utf8(git(dir, args, b""))
        .unwrap()
        .trim()
        .to_owned()
}

#[test]
fn ordinary_profiles_cannot_claim_or_carry_a_legacy_archive() {
    let mut ordinary = Manifest::lifecycle("st-test".into());
    ordinary.legacy_digest = Some("a".repeat(64));
    assert!(ordinary.validate().is_err());

    let migrated = Manifest::migrated("st-test".into(), "A".repeat(64));
    assert!(migrated.validate().is_err());

    let mut caps = Manifest::migrated("st-test".into(), "a".repeat(64));
    caps.capabilities = MIGRATED_ROOT_CAPABILITIES[..4]
        .iter()
        .map(|value| (*value).to_owned())
        .collect();
    assert!(caps.validate().is_err());
}

#[test]
fn malformed_migrated_roots_are_refused_before_any_snapshot_is_returned() {
    let manifest = Manifest::migrated("st-test".into(), "a".repeat(64));
    assert!(
        validate_chain(
            &[migrated_root(manifest, b"not a legacy pack".to_vec())],
            None,
            None
        )
        .is_err()
    );

    let manifest = Manifest::migrated("st-test".into(), "a".repeat(64));
    let mut root = migrated_root(manifest, b"not a legacy pack".to_vec());
    root.files.get_mut("legacy.pack").unwrap().mode = "100755".into();
    assert!(validate_chain(&[root], None, None).is_err());
}

#[test]
fn migrated_root_requires_the_exact_two_regular_root_files() {
    let manifest = Manifest::migrated("st-test".into(), "a".repeat(64));
    let mut root = migrated_root(manifest, b"not a legacy pack".to_vec());
    root.files.insert("extra".into(), blob(b"x".to_vec()));
    assert!(validate_chain(&[root], None, None).is_err());

    let mut ordinary = Manifest::lifecycle("st-test".into());
    ordinary.legacy_digest = None;
    let root = GitCommit {
        oid: "1".repeat(40),
        parents: vec![],
        files: BTreeMap::from([
            ("manifest.json".into(), blob(encode(&ordinary).unwrap())),
            ("legacy.pack".into(), blob(b"unexpected".to_vec())),
        ]),
    };
    assert!(validate_chain(&[root], None, None).is_err());
}

#[test]
fn ordinary_binary_refuses_a_retained_migrated_root_before_remote_mutation() {
    let root = TempDir::new().unwrap();
    let repo = root.path().join("repo");
    let bare = root.path().join("remote.git");
    assert!(
        Command::new("git")
            .current_dir(root.path())
            .args(["init", repo.to_str().unwrap()])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(root.path())
            .args(["init", "--bare", bare.to_str().unwrap()])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&repo)
            .args(["remote", "add", "origin", bare.to_str().unwrap()])
            .status()
            .unwrap()
            .success()
    );

    let manifest = encode(&Manifest::migrated("st-test".into(), "a".repeat(64))).unwrap();
    let pack = b"not a supported retained archive".to_vec();
    let manifest_oid = String::from_utf8(git(&repo, &["hash-object", "-w", "--stdin"], &manifest))
        .unwrap()
        .trim()
        .to_owned();
    let pack_oid = String::from_utf8(git(&repo, &["hash-object", "-w", "--stdin"], &pack))
        .unwrap()
        .trim()
        .to_owned();
    git(&repo, &["read-tree", "--empty"], b"");
    git(
        &repo,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "100644",
            &manifest_oid,
            "manifest.json",
        ],
        b"",
    );
    git(
        &repo,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "100644",
            &pack_oid,
            "legacy.pack",
        ],
        b"",
    );
    let tree_oid = text(&repo, &["write-tree"]);
    let commit_oid = String::from_utf8(git(
        &repo,
        &["-c", "commit.gpgsign=false", "commit-tree", &tree_oid],
        b"Mote Git authority transaction\n",
    ))
    .unwrap()
    .trim()
    .to_owned();
    let artifact = RootArtifact {
        object_format: "sha1".into(),
        manifest_oid,
        manifest_bytes: manifest,
        tree_oid: tree_oid.clone(),
        tree_bytes: git(&repo, &["cat-file", "tree", &tree_oid], b""),
        commit_oid: commit_oid.clone(),
        commit_bytes: git(&repo, &["cat-file", "commit", &commit_oid], b""),
        legacy_pack: Some(RootBlob {
            oid: pack_oid,
            bytes: pack,
        }),
    };
    let transport = mote::git_authority::transport::GitTransport::open(
        &repo,
        "origin",
        "authority",
        &root.path().join("scratch"),
    )
    .unwrap();
    assert!(transport.import_root(&artifact).is_err());
    assert!(transport.head().unwrap().is_none());
}
