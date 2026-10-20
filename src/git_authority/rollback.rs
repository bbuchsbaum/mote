//! Explicit pre-use rollback into one new private local store. The original
//! authority and local store remain permanently attached/retired.
use super::{
    backup,
    cache::atomic_write,
    client::{Client, SubmissionStatus},
    connection::{self, MigrationActivation},
    coordination::{Action, Retirement},
    history::{Binding, Receipt},
    protocol::{Command, OutcomeStatus, Request, decode, encode, invalid, valid_oid},
    transport::GitTransport,
};
use crate::{MoteError, MoteResult, Store};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    device: u64,
    inode: u64,
}
fn identity(path: &Path) -> MoteResult<Identity> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() {
        return invalid("rollback directory identity is not regular");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.permissions().mode() & 0o077 != 0 {
            return invalid("rollback directory is not private");
        }
        Ok(Identity {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        invalid("development rollback requires Unix filesystem identities")
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Retiring,
    Retired,
    Copying,
    Authorized,
    Publishing,
    Completed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    binding: Binding,
    receipt: Receipt,
    retirement: Retirement,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    nonce: String,
    executor_profile: String,
    source: MigrationActivation,
    destination: PathBuf,
    working: PathBuf,
    request: Request,
    phase: Phase,
    proof: Option<Proof>,
    destination_identity: Option<Identity>,
    working_identity: Option<Identity>,
}
impl Journal {
    fn validate(&self) -> MoteResult<()> {
        self.source.descriptor.validate()?;
        self.request.validate()?;
        let Action::AuthorityRetire {
            legacy_digest,
            reason,
        } = retirement_action(&self.request)?
        else {
            return invalid("rollback journal does not contain a retirement request");
        };
        if self.version != 1
            || self.nonce.parse::<ulid::Ulid>().is_err()
            || self.executor_profile.is_empty()
            || !self.destination.is_absolute()
            || self.destination.file_name() != Some(std::ffi::OsStr::new(".mote"))
            || !self.working.is_absolute()
            || legacy_digest != &self.source.migration.export_digest
            || (self.source.descriptor.capabilities
                != super::protocol::RECOVERABLE_MIGRATED_CAPABILITIES
                && self.source.descriptor.capabilities
                    != super::protocol::MIGRATED_COMMUNICATION_CAPABILITIES)
            || self.working != working_path(&self.destination, &self.nonce)?
            || (self.phase != Phase::Retiring && self.proof.is_none())
            || (matches!(
                self.phase,
                Phase::Copying | Phase::Authorized | Phase::Publishing | Phase::Completed
            ) && (self.destination_identity.is_none() || self.working_identity.is_none()))
        {
            return invalid("inconsistent retained rollback identity or phase");
        }
        if let Some(proof) = &self.proof {
            let expected = Retirement {
                request_id: self.request.request_id.clone(),
                legacy_digest: legacy_digest.clone(),
                actor: self.request.actor.clone(),
                session: self.request.session.clone(),
                reason: reason.clone(),
                token: format!("eff-{}-000000", self.request.request_id),
                timestamp: self.request.timestamp.clone(),
            };
            if proof.binding
                != self
                    .source
                    .descriptor
                    .binding(self.source.endpoint_digest.clone())
                || proof.receipt.sequence != 1
                || !valid_oid(&proof.receipt.commit)
                || proof.receipt.request_digest != self.request.digest()?
                || proof.receipt.outcome.status != OutcomeStatus::Accepted
                || proof.receipt.outcome.result["code"] != "authority_retired"
                || proof.receipt.outcome.result["legacy_digest"].as_str() != Some(legacy_digest)
                || proof.retirement != expected
            {
                return invalid(
                    "rollback retirement proof differs from the pinned request and authority",
                );
            }
        }
        Ok(())
    }
}
fn retirement_action(request: &Request) -> MoteResult<&Action> {
    match &request.command {
        Command::Coordination { action } => Ok(action),
        _ => invalid("rollback request is not coordination"),
    }
}

fn journal_path(store: &Store) -> PathBuf {
    store.local_dir().join("git-rollback.json")
}
fn load(store: &Store) -> MoteResult<Option<Journal>> {
    let bytes = match fs::read(journal_path(store)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let journal: Journal = decode(&bytes)?;
    if encode(&journal)? != bytes {
        return invalid("rollback journal is not canonical");
    }
    journal.validate()?;
    Ok(Some(journal))
}
fn save(store: &Store, journal: &Journal) -> MoteResult<()> {
    journal.validate()?;
    atomic_write(&journal_path(store), &encode(journal)?)
}
fn destination_path(path: &Path) -> MoteResult<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let parent = absolute
        .parent()
        .ok_or_else(|| MoteError::Invalid("rollback output needs a parent".into()))?
        .canonicalize()?;
    let leaf = absolute
        .file_name()
        .ok_or_else(|| MoteError::Invalid("rollback output needs a new directory name".into()))?;
    if leaf != std::ffi::OsStr::new(".mote") {
        return invalid(
            "rollback output must name a new .mote directory so MOTE_STORE and --store can open it",
        );
    }
    Ok(parent.join(leaf))
}
fn working_path(destination: &Path, nonce: &str) -> MoteResult<PathBuf> {
    Ok(destination
        .parent()
        .ok_or_else(|| MoteError::Invalid("rollback output needs a parent".into()))?
        .join(format!(".mote-rollback-{nonce}.work")))
}
fn disjoint(a: &Path, b: &Path) -> bool {
    !a.starts_with(b) && !b.starts_with(a)
}
fn check_paths(
    store: &Store,
    transport: &GitTransport,
    source: &MigrationActivation,
    destination: &Path,
    working: &Path,
) -> MoteResult<()> {
    let project = store
        .root()
        .canonicalize()?
        .parent()
        .ok_or_else(|| MoteError::Invalid("rollback source needs a parent".into()))?
        .to_path_buf();
    for path in [destination, working] {
        if transport.checked_durable_path(path)? != path
            || !disjoint(path, &project)
            || !disjoint(path, &source.migration.backup.path)
            || !disjoint(path, &source.cache_root)
        {
            return invalid(
                "rollback output/staging must be disjoint from source project, backup, cache and scratch",
            );
        }
    }
    if !disjoint(destination, working) {
        return invalid("rollback output overlaps its staging directory");
    }
    Ok(())
}
fn check_arguments(journal: &Journal, output: &Path, reason: &str) -> MoteResult<()> {
    let Action::AuthorityRetire {
        reason: expected, ..
    } = retirement_action(&journal.request)?
    else {
        unreachable!()
    };
    if destination_path(output)? != journal.destination || reason != expected {
        return invalid(
            "rollback arguments differ from retained output/reason; resume the original request",
        );
    }
    Ok(())
}
#[derive(Debug, Clone, Serialize)]
pub struct RollbackReceipt {
    pub schema: String,
    pub output: PathBuf,
    pub request_id: String,
    pub binding: Binding,
    pub retirement: Receipt,
    pub previously_completed: bool,
}
fn receipt(journal: &Journal, previously_completed: bool) -> MoteResult<RollbackReceipt> {
    if journal.phase != Phase::Completed
        || identity(&journal.destination)?
            != *journal
                .destination_identity
                .as_ref()
                .expect("validated identity")
    {
        return invalid("rollback is not complete at its pinned destination");
    }
    let proof = journal.proof.as_ref().expect("validated proof");
    Ok(RollbackReceipt {
        schema: "mote.git-rollback-receipt.v1".into(),
        output: journal.destination.clone(),
        request_id: journal.request.request_id.clone(),
        binding: proof.binding.clone(),
        retirement: proof.receipt.clone(),
        previously_completed,
    })
}
/// Report durable prior completion without consulting Git configuration, the
/// network or backup files, and without repairing/overwriting later local work.
pub fn completed(
    store: &Store,
    output: &Path,
    reason: &str,
) -> MoteResult<Option<RollbackReceipt>> {
    if let Some(journal) = load(store)? {
        check_arguments(&journal, output, reason)?;
        if journal.phase == Phase::Completed {
            return receipt(&journal, true).map(Some);
        }
    }
    Ok(None)
}
pub(crate) fn route_hint(store: &Store) -> MoteResult<(String, String)> {
    let descriptor = match load(store)? {
        Some(journal) => journal.source.descriptor,
        None => connection::migration_activation(store)?.descriptor,
    };
    Ok((descriptor.remote, descriptor.branch))
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Checkpoint {
    IntentDurable,
    RetiredDurable,
    DestinationOwned,
    FileCopied,
    CopyVerified,
    PublicationAuthorized,
    PublicationStarted,
    BeforeFormatPublish,
    FormatLinked,
    FormatDurable,
    Completed,
}

pub fn run(
    store: &Store,
    transport: GitTransport,
    output: &Path,
    actor: Option<&str>,
    reason: &str,
) -> MoteResult<RollbackReceipt> {
    run_with_hook(store, transport, output, actor, reason, &mut |_| Ok(()))
}
fn run_with_hook(
    store: &Store,
    transport: GitTransport,
    output: &Path,
    actor: Option<&str>,
    reason: &str,
    hook: &mut dyn FnMut(Checkpoint) -> MoteResult<()>,
) -> MoteResult<RollbackReceipt> {
    if !cfg!(unix) {
        return invalid("development rollback requires Unix filesystem identities");
    }
    let store = Store::open(&store.root().canonicalize()?)?;
    let source_lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(store.local_dir().join("publication.lock"))?;
    source_lock.lock_exclusive()?;
    let mut journal = if let Some(journal) = load(&store)? {
        check_arguments(&journal, output, reason)?;
        if journal.phase == Phase::Completed {
            return receipt(&journal, true);
        }
        journal
    } else {
        let destination = destination_path(output)?;
        let executor_profile = super::legacy_profile::current()?;
        let source = connection::checked_rollback_source(&store, &transport)?;
        match fs::symlink_metadata(&destination) {
            Ok(_) => return invalid("rollback output already exists"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let nonce = ulid::Ulid::new().to_string();
        let working = working_path(&destination, &nonce)?;
        match fs::symlink_metadata(&working) {
            Ok(_) => return invalid("rollback staging directory already exists"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        check_paths(&store, &transport, &source, &destination, &working)?;
        let request = Request {
            version: 1,
            request_id: format!("rollback-{nonce}"),
            actor: store.resolve_actor(actor)?,
            session: format!("rollback-{nonce}"),
            timestamp: crate::ids::format_rfc3339(jiff::Timestamp::now()),
            command: Command::Coordination {
                action: Action::AuthorityRetire {
                    legacy_digest: source.migration.export_digest.clone(),
                    reason: reason.into(),
                },
            },
        };
        let journal = Journal {
            version: 1,
            nonce,
            executor_profile,
            source,
            destination,
            working,
            request,
            phase: Phase::Retiring,
            proof: None,
            destination_identity: None,
            working_identity: None,
        };
        save(&store, &journal)?;
        hook(Checkpoint::IntentDurable)?;
        journal
    };
    if super::legacy_profile::current()? != journal.executor_profile {
        return invalid("rollback requires the exact retained executor profile");
    }
    check_paths(
        &store,
        &transport,
        &journal.source,
        &journal.destination,
        &journal.working,
    )?;
    if transport.endpoint_digest() != journal.source.endpoint_digest
        || transport.branch_ref()
            != journal
                .source
                .descriptor
                .binding(journal.source.endpoint_digest.clone())
                .branch_ref
    {
        return invalid("rollback endpoint or branch differs from retained identity");
    }
    if journal.phase == Phase::Retiring {
        if connection::checked_rollback_source(&store, &transport)? != journal.source {
            return invalid("rollback source changed since request preparation");
        }
        let binding = journal
            .source
            .descriptor
            .binding(journal.source.endpoint_digest.clone());
        let (client, _) = Client::connect_expected(
            transport,
            &journal.source.cache_root,
            &binding,
            &journal.source.descriptor.capabilities,
            None,
        )?;
        let submitted = client.submit(journal.request.clone(), 4)?;
        if submitted.status != SubmissionStatus::Accepted {
            let message = format!(
                "rollback retirement {:?}; request {}; rerun authority rollback-git with the original output and reason; no local copy was activated",
                submitted.status, journal.request.request_id
            );
            return Err(if submitted.status == SubmissionStatus::Rejected {
                MoteError::Rejected(message)
            } else {
                MoteError::Other(message)
            });
        }
        let snapshot = client.refresh()?;
        journal.source.descriptor.check_snapshot(&snapshot)?;
        let confirmed = snapshot
            .receipts
            .get(&journal.request.request_id)
            .ok_or_else(|| {
                MoteError::Invalid(
                    "rollback receipt is not confirmed in fresh canonical history".into(),
                )
            })?;
        let retirement = snapshot.state.coordination.retirement().ok_or_else(|| {
            MoteError::Invalid("canonical authority is not permanently retired".into())
        })?;
        journal.proof = Some(Proof {
            binding,
            receipt: confirmed.clone(),
            retirement: retirement.clone(),
        });
        journal.phase = Phase::Retired;
        save(&store, &journal)?;
        hook(Checkpoint::RetiredDurable)?;
    }
    restore(&store, &mut journal, hook)?;
    receipt(&journal, false)
}
fn restore(
    store: &Store,
    journal: &mut Journal,
    hook: &mut dyn FnMut(Checkpoint) -> MoteResult<()>,
) -> MoteResult<()> {
    journal.validate()?;
    if journal.phase == Phase::Retired {
        backup::verify(&journal.source.migration.backup)?;
        // Creation is exclusive. A crash before pinned ownership is durable
        // leaves an ambiguous destination that refuses rather than being reused.
        backup::private_directory(&journal.destination)?;
        backup::private_directory(&journal.working)?;
        crate::publish::fsync_dir(journal.destination.parent().expect("output parent"))?;
        journal.destination_identity = Some(identity(&journal.destination)?);
        journal.working_identity = Some(identity(&journal.working)?);
        journal.phase = Phase::Copying;
        save(store, journal)?;
        hook(Checkpoint::DestinationOwned)?;
    }
    if identity(&journal.destination)?
        != *journal
            .destination_identity
            .as_ref()
            .ok_or_else(|| MoteError::Invalid("rollback output ownership is not pinned".into()))?
        || identity(&journal.working)?
            != *journal.working_identity.as_ref().ok_or_else(|| {
                MoteError::Invalid("rollback staging ownership is not pinned".into())
            })?
    {
        return invalid("rollback output or staging directory was replaced");
    }
    if journal.phase == Phase::Copying {
        let inventory = backup::copy_without_format(
            &journal.source.migration.backup,
            &journal.destination,
            &journal.working,
            &mut || hook(Checkpoint::FileCopied),
        )?;
        let destination_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(journal.destination.join("local/publication.lock"))?;
        destination_lock.lock_exclusive()?;
        backup::verify_unpublished_copy(&inventory, &journal.destination)?;
        backup::prepare_format(&journal.source.migration.backup, &journal.working)?;
        hook(Checkpoint::CopyVerified)?;
        journal.phase = Phase::Authorized;
        save(store, journal)?;
        hook(Checkpoint::PublicationAuthorized)?;
        begin_publication(store, journal, hook)?;
        return Ok(());
    }
    if matches!(journal.phase, Phase::Authorized | Phase::Publishing) {
        let destination_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(journal.destination.join("local/publication.lock"))?;
        destination_lock.lock_exclusive()?;
        if journal.phase == Phase::Authorized {
            begin_publication(store, journal, hook)?;
        } else {
            // This phase may have exposed a usable local store. Missing FORMAT
            // is damage or an ambiguous attempt, never permission to recreate.
            finalize_publication(store, journal, hook)?;
        }
    }
    Ok(())
}
fn begin_publication(
    store: &Store,
    journal: &mut Journal,
    hook: &mut dyn FnMut(Checkpoint) -> MoteResult<()>,
) -> MoteResult<()> {
    let inventory = backup::verify(&journal.source.migration.backup)?;
    backup::verify_unpublished_copy(&inventory, &journal.destination)?;
    let complete = backup::verify_prepared_format(
        &inventory,
        &journal.working,
        &journal.source.original_format,
    )?;
    journal.phase = Phase::Publishing;
    save(store, journal)?;
    hook(Checkpoint::PublicationStarted)?;
    hook(Checkpoint::BeforeFormatPublish)?;
    fs::hard_link(complete, journal.destination.join("FORMAT.json"))?;
    hook(Checkpoint::FormatLinked)?;
    finalize_publication(store, journal, hook)
}
fn finalize_publication(
    store: &Store,
    journal: &mut Journal,
    hook: &mut dyn FnMut(Checkpoint) -> MoteResult<()>,
) -> MoteResult<()> {
    let destination = journal.destination.join("FORMAT.json");
    let metadata = fs::symlink_metadata(&destination).map_err(|_| MoteError::Invalid(
        "rollback publication may have occurred but FORMAT is absent; reconciliation required, never recreate".into()))?;
    if !metadata.file_type().is_file() || fs::read(&destination)? != journal.source.original_format
    {
        return invalid(
            "rollback FORMAT changed after possible publication; reconciliation required, never recopy",
        );
    }
    crate::publish::fsync_dir(&journal.destination)?;
    hook(Checkpoint::FormatDurable)?;
    journal.phase = Phase::Completed;
    save(store, journal)?;
    hook(Checkpoint::Completed)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        process::{Command as ProcessCommand, Stdio},
        thread,
        time::{Duration, Instant},
    };
    struct Fixture {
        td: tempfile::TempDir,
        store: Store,
        journal: Journal,
    }
    fn fixture() -> Fixture {
        let td = tempfile::tempdir().unwrap();
        let project = td.path().join("project");
        fs::create_dir(&project).unwrap();
        let store = Store::init(&project).unwrap();
        crate::publish::publish_value(
            &store,
            json!({"kind":"create","actor":"alice","entity":"bd-before","set":{"title":"before"}}),
            "2026-01-01T00:00:00Z".parse().unwrap(),
        )
        .unwrap();
        fs::write(
            store.local_dir().join("fixture-private"),
            b"private synthetic bytes\0\xff",
        )
        .unwrap();
        fs::create_dir(store.local_dir().join("empty-fixture")).unwrap();
        let source_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(store.local_dir().join("publication.lock"))
            .unwrap();
        source_lock.lock_exclusive().unwrap();
        let custody = backup::create(&store, &td.path().join("backup")).unwrap();
        drop(source_lock);
        let original_format = fs::read(store.format_path()).unwrap();
        let descriptor = connection::Descriptor {
            version: 1,
            remote: "origin".into(),
            branch: "mote-state".into(),
            store_id: store.read_format().unwrap().store_id,
            generation: 1,
            root: "1".repeat(40),
            capabilities: super::super::protocol::RECOVERABLE_MIGRATED_CAPABILITIES
                .iter()
                .map(|s| s.to_string())
                .collect(),
        };
        let legacy_digest = "e".repeat(64);
        let nonce = ulid::Ulid::new().to_string();
        let parent = td.path().canonicalize().unwrap().join("restored");
        backup::private_directory(&parent).unwrap();
        let destination = parent.join(".mote");
        let request = Request {
            version: 1,
            request_id: format!("rollback-{nonce}"),
            actor: "alice".into(),
            session: format!("rollback-{nonce}"),
            timestamp: "2026-10-20T00:00:00Z".into(),
            command: Command::Coordination {
                action: Action::AuthorityRetire {
                    legacy_digest: legacy_digest.clone(),
                    reason: "fixture restore".into(),
                },
            },
        };
        let retirement = Retirement {
            request_id: request.request_id.clone(),
            legacy_digest: legacy_digest.clone(),
            actor: request.actor.clone(),
            session: request.session.clone(),
            reason: "fixture restore".into(),
            token: format!("eff-{}-000000", request.request_id),
            timestamp: request.timestamp.clone(),
        };
        let proof = Proof {
            binding: descriptor.binding("a".repeat(64)),
            receipt: Receipt {
                commit: "2".repeat(40),
                sequence: 1,
                request_digest: request.digest().unwrap(),
                outcome: super::super::protocol::Outcome {
                    status: OutcomeStatus::Accepted,
                    result: json!({"code":"authority_retired","legacy_digest":legacy_digest}),
                },
            },
            retirement,
        };
        // The restoration engine is tested AFTER a separately validated proof.
        // These private fixtures do not bypass profile checks in the public API.
        let source = MigrationActivation {
            version: 1,
            migration: connection::MigrationSetup {
                backup: custody,
                export_digest: legacy_digest,
                cleanup_start: 1,
            },
            original_format,
            proposed_format: vec![],
            source_fingerprint: "f".repeat(64),
            endpoint_digest: "a".repeat(64),
            cache_root: store.local_dir().join("git-authority"),
            descriptor,
        };
        let journal = Journal {
            version: 1,
            nonce: nonce.clone(),
            executor_profile: "fixture-profile".into(),
            source,
            destination: destination.clone(),
            working: working_path(&destination, &nonce).unwrap(),
            request,
            phase: Phase::Retired,
            proof: Some(proof),
            destination_identity: None,
            working_identity: None,
        };
        save(&store, &journal).unwrap();
        Fixture { td, store, journal }
    }
    #[test]
    fn new_target_restore_is_private_complete_and_never_recopies_after_publication() {
        let mut f = fixture();
        assert!(f.store.require_local_authority().is_err());
        let mut failed = false;
        assert!(
            restore(&f.store, &mut f.journal, &mut |point| {
                if point == Checkpoint::FileCopied && !failed {
                    failed = true;
                    invalid("injected copy interruption")
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        assert!(!f.journal.destination.join("FORMAT.json").exists());
        f.journal = load(&f.store).unwrap().unwrap();
        restore(&f.store, &mut f.journal, &mut |_| Ok(())).unwrap();
        assert_eq!(f.journal.phase, Phase::Completed);
        let target = Store::open(&f.journal.destination).unwrap();
        assert_eq!(
            fs::read(target.local_dir().join("fixture-private")).unwrap(),
            b"private synthetic bytes\0\xff"
        );
        assert!(target.local_dir().join("empty-fixture").is_dir());
        let op = crate::publish::publish_value(
            &target,
            json!({"kind":"create","actor":"alice","entity":"bd-after","set":{"title":"after"}}),
            "2026-10-20T00:00:01Z".parse().unwrap(),
        )
        .unwrap();
        let bytes = fs::read(target.ops_dir().join(format!("{}.json", op.as_str()))).unwrap();
        let report = completed(&f.store, target.root(), "fixture restore")
            .unwrap()
            .unwrap();
        assert!(report.previously_completed);
        assert_eq!(
            fs::read(target.ops_dir().join(format!("{}.json", op.as_str()))).unwrap(),
            bytes
        );
        assert!(
            crate::reducer::replay_store(&target)
                .unwrap()
                .was_accepted(op.as_str())
        );
    }
    #[test]
    fn competing_destination_and_changed_published_format_refuse_without_repair() {
        let mut f = fixture();
        fs::create_dir(&f.journal.destination).unwrap();
        fs::write(f.journal.destination.join("foreign"), b"keep").unwrap();
        assert!(restore(&f.store, &mut f.journal, &mut |_| Ok(())).is_err());
        assert_eq!(
            fs::read(f.journal.destination.join("foreign")).unwrap(),
            b"keep"
        );
        assert!(!f.journal.destination.join("FORMAT.json").exists());
        let mut g = fixture();
        assert!(
            restore(
                &g.store,
                &mut g.journal,
                &mut |p| if p == Checkpoint::FormatLinked {
                    invalid("stop after publication")
                } else {
                    Ok(())
                }
            )
            .is_err()
        );
        let changed = b"changed published format";
        fs::write(g.journal.destination.join("FORMAT.json"), changed).unwrap();
        g.journal = load(&g.store).unwrap().unwrap();
        assert!(restore(&g.store, &mut g.journal, &mut |_| Ok(())).is_err());
        assert_eq!(
            fs::read(g.journal.destination.join("FORMAT.json")).unwrap(),
            changed
        );
    }
    #[test]
    fn proof_and_destination_arguments_are_bound_before_restoration() {
        let f = fixture();
        let mut altered = f.journal.clone();
        altered.proof.as_mut().unwrap().receipt.request_digest = "b".repeat(64);
        assert!(altered.validate().is_err());
        let mut altered = f.journal.clone();
        altered.proof.as_mut().unwrap().binding.root = "3".repeat(40);
        assert!(altered.validate().is_err());
        let mut altered = f.journal.clone();
        altered.proof.as_mut().unwrap().receipt.outcome.status = OutcomeStatus::Rejected;
        assert!(altered.validate().is_err());
        assert!(
            check_arguments(&f.journal, &f.td.path().join("other"), "fixture restore").is_err()
        );
        assert!(check_arguments(&f.journal, &f.journal.destination, "changed reason").is_err());
        assert!(!f.journal.destination.exists());
    }

    #[test]
    fn authorized_resume_refuses_damaged_copy_and_changed_staging_mode_without_repair() {
        for damage in ["missing-op", "truncated-op", "staging-mode"] {
            let mut f = fixture();
            assert!(
                restore(
                    &f.store,
                    &mut f.journal,
                    &mut |p| if p == Checkpoint::PublicationAuthorized {
                        invalid("stop authorized")
                    } else {
                        Ok(())
                    }
                )
                .is_err()
            );
            let op = fs::read_dir(f.journal.destination.join("ops"))
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            match damage {
                "missing-op" => fs::remove_file(&op).unwrap(),
                "truncated-op" => fs::write(&op, b"partial copied op").unwrap(),
                _ => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(
                            f.journal.working.join("FORMAT.complete"),
                            fs::Permissions::from_mode(0o777),
                        )
                        .unwrap();
                    }
                }
            }
            f.journal = load(&f.store).unwrap().unwrap();
            assert!(
                restore(&f.store, &mut f.journal, &mut |_| Ok(())).is_err(),
                "{damage}"
            );
            assert!(!f.journal.destination.join("FORMAT.json").exists());
            assert_eq!(load(&f.store).unwrap().unwrap().phase, Phase::Authorized);
            if damage == "missing-op" {
                assert!(!op.exists());
            }
            if damage == "truncated-op" {
                assert_eq!(fs::read(op).unwrap(), b"partial copied op");
            }
        }
    }
    #[test]
    fn incomplete_prepublication_staging_and_mode_are_regenerated_from_custody() {
        for partial in [true, false] {
            let mut f = fixture();
            assert!(
                restore(
                    &f.store,
                    &mut f.journal,
                    &mut |p| if p == Checkpoint::FileCopied {
                        invalid("stop copying")
                    } else {
                        Ok(())
                    }
                )
                .is_err()
            );
            let complete = f.journal.working.join("FORMAT.complete");
            fs::write(
                &complete,
                if partial {
                    b"partial".as_slice()
                } else {
                    f.journal.source.original_format.as_slice()
                },
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&complete, fs::Permissions::from_mode(0o777)).unwrap();
            }
            f.journal = load(&f.store).unwrap().unwrap();
            restore(&f.store, &mut f.journal, &mut |_| Ok(())).unwrap();
            assert_eq!(
                fs::read(f.journal.destination.join("FORMAT.json")).unwrap(),
                f.journal.source.original_format
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let expected = backup::verify(&f.journal.source.migration.backup)
                    .unwrap()
                    .files["FORMAT.json"]
                    .mode;
                assert_eq!(
                    fs::metadata(f.journal.destination.join("FORMAT.json"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    expected
                );
            }
        }
    }
    #[test]
    fn removed_format_after_linking_is_damage_not_permission_to_recreate() {
        let mut f = fixture();
        assert!(
            restore(
                &f.store,
                &mut f.journal,
                &mut |p| if p == Checkpoint::FormatLinked {
                    invalid("stop linked")
                } else {
                    Ok(())
                }
            )
            .is_err()
        );
        fs::remove_file(f.journal.destination.join("FORMAT.json")).unwrap();
        f.journal = load(&f.store).unwrap().unwrap();
        assert_eq!(f.journal.phase, Phase::Publishing);
        assert!(restore(&f.store, &mut f.journal, &mut |_| Ok(())).is_err());
        assert!(!f.journal.destination.join("FORMAT.json").exists());
        assert_eq!(load(&f.store).unwrap().unwrap().phase, Phase::Publishing);
    }
    #[test]
    #[ignore]
    fn restore_worker() {
        let root = std::env::var_os("MOTE_RESTORE_FIXTURE").unwrap();
        let store = Store::open(Path::new(&root)).unwrap();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(store.local_dir().join("publication.lock"))
            .unwrap();
        lock.lock_exclusive().unwrap();
        let mut journal = load(&store).unwrap().unwrap();
        let point = std::env::var("MOTE_RESTORE_STOP").unwrap();
        let marker = PathBuf::from(std::env::var_os("MOTE_RESTORE_MARKER").unwrap());
        restore(&store, &mut journal, &mut |p| {
            if format!("{p:?}") == point {
                fs::write(&marker, b"ready")?;
                loop {
                    thread::park();
                }
            }
            Ok(())
        })
        .unwrap();
    }
    #[test]
    #[ignore]
    fn destination_writer_worker() {
        let root = std::env::var_os("MOTE_RESTORE_FIXTURE").unwrap();
        let store = Store::open(Path::new(&root)).unwrap();
        let marker = PathBuf::from(std::env::var_os("MOTE_RESTORE_MARKER").unwrap());
        fs::write(marker, b"starting").unwrap();
        crate::publish::publish_value(
            &store,
            json!({"kind":"create","actor":"alice","entity":"bd-waited","set":{"title":"waited"}}),
            "2026-10-20T00:00:02Z".parse().unwrap(),
        )
        .unwrap();
    }
    #[test]
    fn process_death_at_restore_boundaries_keeps_lock_inodes_and_later_writes() {
        for point in [
            Checkpoint::DestinationOwned,
            Checkpoint::FileCopied,
            Checkpoint::CopyVerified,
            Checkpoint::PublicationAuthorized,
            Checkpoint::PublicationStarted,
            Checkpoint::BeforeFormatPublish,
            Checkpoint::FormatLinked,
            Checkpoint::FormatDurable,
            Checkpoint::Completed,
        ] {
            let f = fixture();
            let marker = f.td.path().join("paused");
            #[cfg(unix)]
            let source_inode = {
                use std::os::unix::fs::MetadataExt;
                fs::metadata(f.store.local_dir().join("publication.lock"))
                    .unwrap()
                    .ino()
            };
            let mut child = ProcessCommand::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "git_authority::rollback::tests::restore_worker",
                ])
                .env("MOTE_RESTORE_FIXTURE", f.store.root())
                .env("MOTE_RESTORE_STOP", format!("{point:?}"))
                .env("MOTE_RESTORE_MARKER", &marker)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            while !marker.exists() && Instant::now() < deadline {
                if child.try_wait().unwrap().is_some() {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            let paused = marker.exists();
            let mut waiting = None;
            let mut blocked = false;
            if paused && point == Checkpoint::FormatLinked {
                let starting = f.td.path().join("writer-starting");
                let mut writer = ProcessCommand::new(std::env::current_exe().unwrap())
                    .args([
                        "--ignored",
                        "--exact",
                        "git_authority::rollback::tests::destination_writer_worker",
                    ])
                    .env("MOTE_RESTORE_FIXTURE", &f.journal.destination)
                    .env("MOTE_RESTORE_MARKER", &starting)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap();
                let deadline = Instant::now() + Duration::from_secs(5);
                while !starting.exists() && Instant::now() < deadline {
                    if writer.try_wait().unwrap().is_some() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                thread::sleep(Duration::from_millis(30));
                blocked = starting.exists() && writer.try_wait().unwrap().is_none();
                waiting = Some(writer);
            }
            let _ = child.kill();
            let _ = child.wait();
            if let Some(mut writer) = waiting {
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut status = None;
                while Instant::now() < deadline {
                    if let Some(s) = writer.try_wait().unwrap() {
                        status = Some(s);
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                if status.is_none() {
                    let _ = writer.kill();
                    let _ = writer.wait();
                }
                assert!(blocked, "destination writer bypassed held lock");
                assert!(
                    status.is_some_and(|s| s.success()),
                    "waiting writer did not finish"
                );
            }
            assert!(paused, "helper never reached {point:?}");
            let mut journal = load(&f.store).unwrap().unwrap();
            let published = journal.destination.join("FORMAT.json").exists();
            if published && point != Checkpoint::FormatLinked {
                let target = Store::open(&journal.destination).unwrap();
                crate::publish::publish_value(&target,json!({"kind":"create","actor":"alice","entity":"bd-after-death","set":{"title":"survives"}}),"2026-10-20T00:00:03Z".parse().unwrap()).unwrap();
            }
            #[cfg(unix)]
            let destination_inode = {
                use std::os::unix::fs::MetadataExt;
                fs::metadata(journal.destination.join("local/publication.lock"))
                    .ok()
                    .map(|m| m.ino())
            };
            if matches!(
                point,
                Checkpoint::PublicationStarted | Checkpoint::BeforeFormatPublish
            ) {
                assert_eq!(journal.phase, Phase::Publishing);
                assert!(restore(&f.store, &mut journal, &mut |_| Ok(())).is_err());
                assert!(!journal.destination.join("FORMAT.json").exists());
                continue;
            }
            restore(&f.store, &mut journal, &mut |_| Ok(())).unwrap();
            let target = Store::open(&journal.destination).unwrap();
            let state = crate::reducer::replay_store(&target).unwrap();
            assert!(state.beads.contains_key("bd-before"));
            if published {
                assert!(
                    state
                        .beads
                        .contains_key(if point == Checkpoint::FormatLinked {
                            "bd-waited"
                        } else {
                            "bd-after-death"
                        })
                );
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                assert_eq!(
                    fs::metadata(f.store.local_dir().join("publication.lock"))
                        .unwrap()
                        .ino(),
                    source_inode
                );
                if let Some(inode) = destination_inode {
                    assert_eq!(
                        fs::metadata(target.local_dir().join("publication.lock"))
                            .unwrap()
                            .ino(),
                        inode
                    );
                }
            }
        }
    }
}
