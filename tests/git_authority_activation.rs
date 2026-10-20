use mote::{
    Store,
    git_authority::{
        cache::Cache,
        client::SubmissionStatus,
        connection::{self, Descriptor},
        protocol::{Manifest, Request},
        transport::GitTransport,
    },
};
use serde_json::json;
use std::{fs, path::Path, process::Command};
use tempfile::{TempDir, tempdir};
struct Fixture {
    td: TempDir,
    a: Store,
    b: Store,
}
impl Fixture {
    fn new() -> Self {
        let td = tempdir().unwrap();
        let remote = td.path().join("remote.git");
        git(
            td.path(),
            &["init", "--bare", "-q", remote.to_str().unwrap()],
        );
        for name in ["a", "b"] {
            let repo = td.path().join(name);
            fs::create_dir(&repo).unwrap();
            git(&repo, &["init", "-q"]);
            git(
                &repo,
                &["remote", "add", "origin", remote.to_str().unwrap()],
            );
        }
        let a = Store::init(&td.path().join("a")).unwrap();
        let b = Store::init(&td.path().join("b")).unwrap();
        Self { td, a, b }
    }
    fn transport(&self, name: &str, suffix: &str) -> GitTransport {
        GitTransport::open(
            &self.td.path().join(name),
            "origin",
            "mote-state",
            &self.td.path().join(format!("scratch-{suffix}")),
        )
        .unwrap()
    }
    fn cache(&self, name: &str) -> std::path::PathBuf {
        connection::default_cache_root(if name == "a" { &self.a } else { &self.b })
    }
    fn bootstrap(&self) -> Descriptor {
        connection::bootstrap(&self.a, self.transport("a", "bootstrap"), &self.cache("a")).unwrap()
    }
    fn portable_clone_marker(&self) {
        fs::copy(self.a.format_path(), self.b.format_path()).unwrap();
        fs::copy(
            connection::descriptor_path(self.a.root()),
            connection::descriptor_path(self.b.root()),
        )
        .unwrap();
    }
}
fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
#[test]
fn bootstrap_and_explicit_second_machine_connection_share_one_communication_authority() {
    let f = Fixture::new();
    let original_b = f.b.read_format().unwrap().store_id;
    let descriptor = f.bootstrap();
    assert_eq!(
        descriptor.capabilities,
        Manifest::communications(descriptor.store_id.clone()).capabilities
    );
    assert!(!connection::setup_path(f.a.root()).exists());
    assert!(f.a.require_local_authority().is_err());
    assert_ne!(original_b, descriptor.store_id);
    let connected = connection::connect(&f.b, f.transport("b", "connect"), &f.cache("b")).unwrap();
    assert_eq!(connected, descriptor);
    assert_eq!(f.b.read_format().unwrap().store_id, descriptor.store_id);
    let client = connection::open_client(&f.a, f.transport("a", "submit"), &f.cache("a")).unwrap();
    let request:Request=serde_json::from_value(json!({"version":1,"request_id":"enroll","actor":"alice","session":"sess-a","timestamp":"2026-10-20T00:00:00Z","command":{"kind":"coordination","action":{"kind":"session.start","label":"first machine"}}})).unwrap();
    assert_eq!(
        client.submit(request, 4).unwrap().status,
        SubmissionStatus::Accepted
    );
    let other = connection::open_client(&f.b, f.transport("b", "read"), &f.cache("b")).unwrap();
    let snapshot = other.refresh().unwrap();
    assert_eq!(snapshot.state.sequence, 1);
    assert_eq!(
        snapshot.state.coordination.view().unwrap()["sessions"]["sess-a"]["actor"],
        "alice"
    );
    assert_eq!(fs::read_dir(f.a.ops_dir()).unwrap().count(), 0);
    assert_eq!(fs::read_dir(f.b.ops_dir()).unwrap().count(), 0);
}
#[test]
fn portable_descriptor_requires_explicit_machine_connection_and_keeps_local_alias() {
    let f = Fixture::new();
    let descriptor = f.bootstrap();
    f.portable_clone_marker();
    assert!(connection::open_client(&f.b, f.transport("b", "unconnected"), &f.cache("b")).is_err());
    git(
        &f.td.path().join("b"),
        &[
            "remote",
            "add",
            "upstream",
            f.td.path().join("remote.git").to_str().unwrap(),
        ],
    );
    let transport = GitTransport::open(
        &f.td.path().join("b"),
        "upstream",
        "mote-state",
        &f.td.path().join("upstream-scratch"),
    )
    .unwrap();
    assert_eq!(
        connection::connect(&f.b, transport, &f.cache("b")).unwrap(),
        descriptor
    );
    assert_eq!(
        connection::selected_remote(&f.b, &f.cache("b")).unwrap(),
        "upstream"
    );
    assert_eq!(
        connection::load_descriptor(f.b.root())
            .unwrap()
            .unwrap()
            .remote,
        "origin"
    );
}
#[test]
fn selected_profile_mismatch_refuses_before_cache_binding_or_high_water_advancement() {
    let f = Fixture::new();
    let mut descriptor = f.bootstrap();
    f.portable_clone_marker();
    descriptor.capabilities = Manifest::coordination(descriptor.store_id.clone()).capabilities;
    fs::write(
        connection::descriptor_path(f.b.root()),
        mote::git_authority::protocol::encode(&descriptor).unwrap(),
    )
    .unwrap();
    assert!(connection::connect(&f.b, f.transport("b", "mismatch"), &f.cache("b")).is_err());
    let cache = Cache::open(&f.cache("b")).unwrap();
    assert!(cache.binding().unwrap().is_none());
    assert!(cache.previous_head().unwrap().is_none());
    assert!(!connection::setup_path(f.b.root()).exists());
}
#[test]
fn existing_remote_bootstrap_and_nonempty_local_connect_refuse_without_activation() {
    let f = Fixture::new();
    let descriptor = f.bootstrap();
    let original = fs::read(f.b.format_path()).unwrap();
    assert!(connection::bootstrap(&f.b, f.transport("b", "competing"), &f.cache("b")).is_err());
    assert_eq!(fs::read(f.b.format_path()).unwrap(), original);
    assert!(!connection::setup_path(f.b.root()).exists());
    mote::publish::publish_value(
        &f.b,
        json!({"kind":"create","actor":"bob","entity":"bd-local","set":{"title":"local history"}}),
        jiff::Timestamp::now(),
    )
    .unwrap();
    assert!(connection::connect(&f.b, f.transport("b", "nonempty"), &f.cache("b")).is_err());
    assert_eq!(fs::read(f.b.format_path()).unwrap(), original);
    assert!(!connection::descriptor_path(f.b.root()).exists());
    assert_eq!(
        f.transport("a", "verify").head().unwrap(),
        Some(descriptor.root)
    );
}

#[test]
fn activated_profile_is_checked_at_open_refresh_and_before_new_intent() {
    let f = Fixture::new();
    let mut descriptor = f.bootstrap();
    let cache = Cache::open(&f.cache("a")).unwrap();
    let before = cache.previous_head().unwrap();
    descriptor.capabilities = Manifest::coordination(descriptor.store_id.clone()).capabilities;
    fs::write(
        connection::descriptor_path(f.a.root()),
        mote::git_authority::protocol::encode(&descriptor).unwrap(),
    )
    .unwrap();
    assert!(connection::selected_remote(&f.a, &f.cache("a")).is_err());
    assert!(
        connection::open_client(&f.a, f.transport("a", "descriptor-only"), &f.cache("a")).is_err()
    );
    // Even if both local selections are altered consistently, the actual
    // immutable remote manifest remains authoritative before observation/write.
    let route_path = f.cache("a").join("route.json");
    let mut route: serde_json::Value =
        serde_json::from_slice(&fs::read(&route_path).unwrap()).unwrap();
    route["capabilities"] = json!(descriptor.capabilities);
    fs::write(
        route_path,
        mote::git_authority::protocol::encode(&route).unwrap(),
    )
    .unwrap();
    let client =
        connection::open_client(&f.a, f.transport("a", "both-selections"), &f.cache("a")).unwrap();
    assert!(client.refresh().is_err());
    let request: Request = serde_json::from_value(json!({"version":1,"request_id":"wrong-profile","actor":"alice","session":"sess-a","timestamp":"2026-10-20T00:00:00Z","command":{"kind":"coordination","action":{"kind":"session.start","label":"must refuse"}}})).unwrap();
    assert!(client.submit(request, 4).is_err());
    assert_eq!(cache.previous_head().unwrap(), before);
    assert!(!f.cache("a").join("requests/wrong-profile.json").exists());
    assert_eq!(
        f.transport("a", "verify-profile").head().unwrap(),
        Some(descriptor.root)
    );
}

#[test]
fn disposable_scratch_cannot_overlap_durable_cache_in_either_direction() {
    for relation in ["equal", "cache-inside", "scratch-inside", "symlink"] {
        let f = Fixture::new();
        let cache = f.cache("a");
        let scratch = match relation {
            "equal" => cache.clone(),
            "cache-inside" => cache.parent().unwrap().to_path_buf(),
            "scratch-inside" => cache.join("scratch"),
            "symlink" => f.td.path().join("aliased-scratch"),
            _ => unreachable!(),
        };
        fs::create_dir_all(&scratch).unwrap();
        #[cfg(unix)]
        if relation == "symlink" {
            std::os::unix::fs::symlink(&scratch, &cache).unwrap();
        }
        let transport =
            GitTransport::open(&f.td.path().join("a"), "origin", "mote-state", &scratch);
        // Source-local scratch is already rejected at the transport boundary.
        // The symlink case is outside the source and reaches cache validation.
        if let Ok(transport) = transport {
            assert!(
                connection::bootstrap(&f.a, transport, &cache).is_err(),
                "{relation}"
            );
        }
        assert!(!cache.join("binding.json").exists());
        assert!(!connection::setup_path(f.a.root()).exists());
        assert!(f.transport("a", "verify-overlap").head().unwrap().is_none());
    }
}
