use mote::git_authority::{protocol::Manifest, transport::GitTransport};
use std::{path::Path, process::Command};
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .current_dir(dir)
            .args(args)
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn retained_root_survives_scratch_loss_with_exact_object_identity() {
    let root = TempDir::new().unwrap();
    let repo = root.path().join("repo");
    let bare = root.path().join("remote.git");
    git(root.path(), &["init", repo.to_str().unwrap()]);
    git(root.path(), &["init", "--bare", bare.to_str().unwrap()]);
    git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
    let scratch = root.path().join("scratch");
    let transport = GitTransport::open(&repo, "origin", "authority", &scratch).unwrap();
    let manifest = Manifest::new("st-connection".into());
    let artifact = transport.capture_root(&manifest).unwrap();
    std::fs::remove_dir_all(&scratch).unwrap();
    let restored =
        GitTransport::open(&repo, "origin", "authority", &root.path().join("scratch-2")).unwrap();
    restored.import_root(&artifact).unwrap();
    assert_eq!(
        restored.commit(&artifact.commit_oid).unwrap().files["manifest.json"]
            .bytes
            .as_ref(),
        artifact.manifest_bytes
    );
    let mut changed = artifact.clone();
    changed.manifest_bytes.push(b'!');
    assert!(restored.import_root(&changed).is_err());
    let mut mixed = artifact.clone();
    mixed.tree_oid = mixed.manifest_oid.clone();
    assert!(restored.import_root(&mixed).is_err());
    let other = restored
        .capture_root(&Manifest::new("st-different".into()))
        .unwrap();
    let mut mixed_valid_objects = artifact.clone();
    mixed_valid_objects.tree_oid = other.tree_oid;
    mixed_valid_objects.tree_bytes = other.tree_bytes;
    assert!(restored.import_root(&mixed_valid_objects).is_err());
}
