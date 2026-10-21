//! Experimental local client for the reviewed Git transaction profiles.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::cache::{Cache, CachedView, atomic_write, read_optional};
use super::history::{Binding, Receipt, Snapshot, validate_chain};
use super::protocol::{Request, Transaction, encode, invalid, simple_identifier, transaction_path};
use super::transport::{GitTransport, PushFailure};
use crate::{MoteError, MoteResult};

pub struct Client {
    transport: GitTransport,
    cache: Cache,
    expected_capabilities: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalEntry {
    pub version: u32,
    pub request: Request,
    pub attempted_commits: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubmissionStatus {
    Accepted,
    Rejected,
    Unconfirmed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Submission {
    pub status: SubmissionStatus,
    pub request_id: String,
    pub attempted_commit: Option<String>,
    pub receipt: Option<Receipt>,
    pub recovered: bool,
    pub diagnostic: Option<Diagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCause {
    Contention,
    Denied,
    Timeout,
    Unavailable,
    Incompatible,
    LocalState,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub cause: FailureCause,
    pub phase: String,
    pub push_cause: Option<FailureCause>,
}

impl Client {
    /// Explicitly connect a cache to an existing authority. This does not
    /// initialize a branch, migrate a task store, or enable CLI task commands.
    pub fn connect(transport: GitTransport, cache_root: &Path) -> MoteResult<Self> {
        Self::connect_inner(transport, cache_root, None, None).map(|(client, _)| client)
    }

    pub(crate) fn connect_snapshot(
        transport: GitTransport,
        cache_root: &Path,
    ) -> MoteResult<(Self, Snapshot)> {
        Self::connect_inner(transport, cache_root, None, None)
    }

    /// Setup validates the selected identity before binding or advancing its
    /// cache. A resumed setup can also retain a minimum confirmed observation.
    pub(crate) fn connect_expected(
        transport: GitTransport,
        cache_root: &Path,
        expected: &Binding,
        capabilities: &[String],
        minimum_head: Option<&str>,
    ) -> MoteResult<(Self, Snapshot)> {
        Self::connect_inner(
            transport,
            cache_root,
            Some((expected, capabilities)),
            minimum_head,
        )
    }

    fn connect_inner(
        transport: GitTransport,
        cache_root: &Path,
        expected: Option<(&Binding, &[String])>,
        minimum_head: Option<&str>,
    ) -> MoteResult<(Self, Snapshot)> {
        let cache = Cache::open(&transport.checked_cache_path(cache_root)?)?;
        let client = Self {
            transport,
            cache,
            expected_capabilities: expected.map(|(_, caps)| caps.to_vec()),
        };
        let snapshot = {
            let _lock = client.cache.lock()?;
            let existing = client.cache.binding()?;
            if let Some((expected, _)) = expected {
                expected.check_endpoint(
                    &client.transport.endpoint_digest(),
                    client.transport.branch_ref(),
                )?;
                if existing.as_ref().is_some_and(|binding| binding != expected) {
                    return invalid("cache binding differs from selected authority");
                }
            }
            if let Some(binding) = &existing {
                binding.check_endpoint(
                    &client.transport.endpoint_digest(),
                    client.transport.branch_ref(),
                )?;
            }
            let head = client.transport.head()?.ok_or_else(|| {
                MoteError::Invalid(
                    "authority branch is missing; explicit initialization is required".into(),
                )
            })?;
            let chain = client.chain(&head)?;
            let previous = client.cache.previous_head()?;
            let binding = expected.map(|(binding, _)| binding).or(existing.as_ref());
            let snapshot = validate_chain(&chain, binding, previous.as_deref())?;
            if minimum_head.is_some_and(|head| !chain.iter().any(|commit| commit.oid == head)) {
                return invalid("authority lost the setup's confirmed observation");
            }
            if expected.is_some_and(|(_, caps)| caps != snapshot.manifest.capabilities) {
                return invalid("authority profile differs from selected capabilities");
            }
            let binding = Binding {
                version: 1,
                endpoint_digest: client.transport.endpoint_digest(),
                branch_ref: client.transport.branch_ref().into(),
                store_id: snapshot.manifest.store_id.clone(),
                generation: snapshot.manifest.generation,
                root: snapshot.root.clone(),
            };
            client.cache.bind(&binding)?;
            client.cache.observe(&snapshot)?;
            snapshot
        };
        Ok((client, snapshot))
    }

    /// Open an explicitly connected machine without an extra remote read.
    /// Subsequent reads/submissions still perform the normal full refresh.
    pub(crate) fn open_bound(
        transport: GitTransport,
        cache_root: &Path,
        expected: &Binding,
        capabilities: &[String],
    ) -> MoteResult<Self> {
        expected.check_endpoint(&transport.endpoint_digest(), transport.branch_ref())?;
        let cache = Cache::open(&transport.checked_cache_path(cache_root)?)?;
        {
            let _lock = cache.lock()?;
            if cache.binding()?.as_ref() != Some(expected) {
                return invalid(
                    "this machine is not connected to the selected authority; run authority connect",
                );
            }
        }
        Ok(Self {
            transport,
            cache,
            expected_capabilities: Some(capabilities.to_vec()),
        })
    }

    pub fn refresh(&self) -> MoteResult<Snapshot> {
        let _lock = self.cache.lock()?;
        self.refresh_locked()
    }

    pub fn cached_view(&self) -> MoteResult<Option<CachedView>> {
        let _lock = self.cache.lock()?;
        self.cache.cached_view()
    }

    /// A missing receipt in one observation does not settle an in-flight push.
    pub fn receipt(&self, request_id: &str) -> MoteResult<Option<Receipt>> {
        if !simple_identifier(request_id, 100) {
            return invalid("invalid request ID");
        }
        Ok(self.refresh()?.receipts.get(request_id).cloned())
    }

    pub fn retry_pending(&self, request_id: &str, max_attempts: usize) -> MoteResult<Submission> {
        let request = {
            let _lock = self.cache.lock()?;
            self.journal(request_id)?
                .ok_or_else(|| MoteError::Invalid("no retained request with that ID".into()))?
                .request
        };
        self.submit(request, max_attempts)
    }

    pub fn submit(&self, request: Request, max_attempts: usize) -> MoteResult<Submission> {
        self.submit_with_persistence(request, max_attempts, &mut |entry| self.persist(entry))
    }

    fn submit_with_persistence(
        &self,
        request: Request,
        max_attempts: usize,
        persist: &mut impl FnMut(&JournalEntry) -> MoteResult<()>,
    ) -> MoteResult<Submission> {
        request.validate()?;
        if !(1..=64).contains(&max_attempts) {
            return invalid("submission attempts must be between 1 and 64");
        }
        let _lock = self.cache.lock()?;
        let mut pending_intent = false;
        let mut journal = match self.journal(&request.request_id)? {
            Some(existing) => {
                if existing.request.digest()? != request.digest()? {
                    return invalid("request ID already belongs to different canonical intent");
                }
                existing
            }
            None => {
                let entry = JournalEntry {
                    version: 1,
                    request: request.clone(),
                    attempted_commits: vec![],
                };
                if self.expected_capabilities.is_some() {
                    // A checked project profile must be validated before a
                    // new intent is retained under that profile.
                    pending_intent = true;
                } else {
                    persist(&entry)?;
                }
                entry
            }
        };
        for attempt in 0..max_attempts {
            let snapshot = match self.refresh_locked() {
                Ok(snapshot) => snapshot,
                Err(error) if journal.attempted_commits.is_empty() => return Err(error),
                Err(error) => {
                    return Ok(unconfirmed(
                        &journal,
                        diagnostic(&error, "refresh_before_retry", None),
                    ));
                }
            };
            if let Some(receipt) = snapshot.receipts.get(&request.request_id) {
                return confirmed(&request, receipt, true);
            }
            if let Err(error) = snapshot.manifest.allows(&request.command) {
                return uncertain_or_error(&journal, error, "profile");
            }
            if pending_intent {
                persist(&journal)?;
                pending_intent = false;
            }
            let transaction =
                match Transaction::prepare(&snapshot.state, &snapshot.head, request.clone()) {
                    Ok(transaction) => transaction,
                    Err(error) => return uncertain_or_error(&journal, error, "plan"),
                };
            let path = transaction_path(transaction.sequence);
            let transaction_bytes = encode(&transaction)?;
            let commit =
                match self
                    .transport
                    .append_commit(&snapshot.head, &path, &transaction_bytes)
                {
                    Ok(commit) => commit,
                    Err(error) if journal.attempted_commits.is_empty() => return Err(error),
                    Err(error) => {
                        return Ok(unconfirmed(&journal, diagnostic(&error, "prepare", None)));
                    }
                };
            // Recording before the push is intentionally conservative: a
            // crash at any later point leaves a recoverable uncertain attempt.
            let mut attempted = journal.clone();
            attempted.attempted_commits.push(commit.clone());
            if let Err(error) = persist(&attempted) {
                // The new proposal has not been pushed. Preserve uncertainty
                // about earlier retained attempts, without reporting this
                // unpersisted proposal as the retained attempt.
                return uncertain_or_error(&journal, error, "journal_attempt");
            }
            journal = attempted;
            let pushed = self.transport.push(&commit);
            let push_cause = match &pushed {
                Ok(attempt) if attempt.success => None,
                Ok(attempt) => Some(match attempt.failure {
                    Some(PushFailure::Contention) => FailureCause::Contention,
                    Some(PushFailure::Denied) => FailureCause::Denied,
                    Some(PushFailure::Timeout) => FailureCause::Timeout,
                    Some(PushFailure::Unavailable) => FailureCause::Unavailable,
                    Some(PushFailure::Unknown) | None => FailureCause::Unknown,
                }),
                Err(error) => Some(error_cause(error)),
            };
            let observed = match self.refresh_locked() {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    return Ok(unconfirmed(
                        &journal,
                        diagnostic(&error, "confirmation", push_cause),
                    ));
                }
            };
            if let Some(receipt) = observed.receipts.get(&request.request_id) {
                let recovered = !pushed.as_ref().is_ok_and(|push| push.success);
                let mut result = confirmed(&request, receipt, recovered)?;
                result.diagnostic = push_cause.map(|cause| Diagnostic {
                    cause,
                    phase: "push_recovered".into(),
                    push_cause: Some(cause),
                });
                return Ok(result);
            }
            if push_cause != Some(FailureCause::Contention)
                || observed.head == snapshot.head
                || attempt + 1 == max_attempts
            {
                return Ok(unconfirmed(
                    &journal,
                    Diagnostic {
                        cause: push_cause.unwrap_or(FailureCause::Unknown),
                        phase: "confirmation".into(),
                        push_cause,
                    },
                ));
            }
            thread::sleep(Duration::from_millis(10 + rand::random::<u64>() % 31));
        }
        Ok(unconfirmed(
            &journal,
            Diagnostic {
                cause: FailureCause::Contention,
                phase: "retry_limit".into(),
                push_cause: Some(FailureCause::Contention),
            },
        ))
    }

    fn refresh_locked(&self) -> MoteResult<Snapshot> {
        let binding = self
            .cache
            .binding()?
            .ok_or_else(|| MoteError::Invalid("authority cache is not bound".into()))?;
        binding.check_endpoint(
            &self.transport.endpoint_digest(),
            self.transport.branch_ref(),
        )?;
        let head = self.transport.head()?.ok_or_else(|| {
            MoteError::Invalid(
                "bound authority branch disappeared; refusing reinitialization or local fallback"
                    .into(),
            )
        })?;
        let chain = self.chain(&head)?;
        let previous = self.cache.previous_head()?;
        let snapshot = validate_chain(&chain, Some(&binding), previous.as_deref())?;
        if self
            .expected_capabilities
            .as_ref()
            .is_some_and(|caps| *caps != snapshot.manifest.capabilities)
        {
            return invalid("authority profile differs from selected capabilities");
        }
        self.cache.observe(&snapshot)?;
        Ok(snapshot)
    }

    fn chain(&self, head: &str) -> MoteResult<Vec<super::transport::GitCommit>> {
        let mut chain = Vec::new();
        let mut blobs = BTreeMap::new();
        let mut oid = head.to_owned();
        let mut seen = BTreeSet::new();
        loop {
            if !seen.insert(oid.clone()) {
                return invalid("authority commit cycle");
            }
            let commit = self.transport.commit_memo(&oid, &mut blobs)?;
            let parent = match commit.parents.as_slice() {
                [] => None,
                [parent] => Some(parent.clone()),
                _ => return invalid("authority merge commits are not supported"),
            };
            chain.push(commit);
            match parent {
                Some(parent) => oid = parent,
                None => break,
            }
        }
        chain.reverse();
        Ok(chain)
    }

    fn journal(&self, id: &str) -> MoteResult<Option<JournalEntry>> {
        if !simple_identifier(id, 100) {
            return invalid("invalid request ID");
        }
        let entry: Option<JournalEntry> = read_optional(
            &self
                .cache
                .root()
                .join("requests")
                .join(format!("{id}.json")),
        )?;
        if let Some(entry) = &entry {
            if entry.version != 1 || entry.request.request_id != id {
                return invalid("retained request identity or journal version mismatch");
            }
            entry.request.validate()?;
            if entry
                .attempted_commits
                .iter()
                .any(|oid| !super::protocol::valid_oid(oid))
            {
                return invalid("invalid retained attempt commit");
            }
        }
        Ok(entry)
    }

    fn persist(&self, entry: &JournalEntry) -> MoteResult<()> {
        atomic_write(
            &self
                .cache
                .root()
                .join("requests")
                .join(format!("{}.json", entry.request.request_id)),
            &encode(entry)?,
        )
    }
}

fn confirmed(request: &Request, receipt: &Receipt, recovered: bool) -> MoteResult<Submission> {
    if receipt.request_digest != request.digest()? {
        return invalid("remote receipt uses this request ID for different intent");
    }
    let status = match receipt.outcome.status {
        super::protocol::OutcomeStatus::Accepted => SubmissionStatus::Accepted,
        super::protocol::OutcomeStatus::Rejected => SubmissionStatus::Rejected,
    };
    Ok(Submission {
        status,
        request_id: request.request_id.clone(),
        attempted_commit: Some(receipt.commit.clone()),
        receipt: Some(receipt.clone()),
        recovered,
        diagnostic: None,
    })
}

fn unconfirmed(journal: &JournalEntry, diagnostic: Diagnostic) -> Submission {
    Submission {
        status: SubmissionStatus::Unconfirmed,
        request_id: journal.request.request_id.clone(),
        attempted_commit: journal.attempted_commits.last().cloned(),
        receipt: None,
        recovered: false,
        diagnostic: Some(diagnostic),
    }
}

fn diagnostic(error: &MoteError, phase: &str, push_cause: Option<FailureCause>) -> Diagnostic {
    Diagnostic {
        cause: error_cause(error),
        phase: phase.into(),
        push_cause,
    }
}

fn uncertain_or_error(
    journal: &JournalEntry,
    error: MoteError,
    phase: &str,
) -> MoteResult<Submission> {
    if journal.attempted_commits.is_empty() {
        Err(error)
    } else {
        Ok(unconfirmed(journal, diagnostic(&error, phase, None)))
    }
}

fn error_cause(error: &MoteError) -> FailureCause {
    match error {
        MoteError::Invalid(_) | MoteError::Json(_) | MoteError::HashMismatch { .. } => {
            FailureCause::Incompatible
        }
        MoteError::Io(_) => FailureCause::LocalState,
        MoteError::Other(message) if message.contains("timed out") => FailureCause::Timeout,
        _ => FailureCause::Unavailable,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    #[test]
    fn retry_journal_sync_failure_preserves_uncertainty_without_another_push() {
        let td = tempfile::tempdir().unwrap();
        let repo = td.path().join("source");
        let remote = td.path().join("remote.git");
        for path in [&repo, &remote] {
            fs::create_dir(path).unwrap();
        }
        for (path, args) in [
            (&repo, vec!["init", "-q"]),
            (&remote, vec!["init", "--bare", "-q"]),
            (
                &repo,
                vec!["remote", "add", "origin", remote.to_str().unwrap()],
            ),
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(path)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let transport =
            GitTransport::open(&repo, "origin", "mote-state", &td.path().join("git")).unwrap();
        let root = transport
            .create_commit(
                None,
                &BTreeMap::from([(
                    "manifest.json".into(),
                    encode(&super::super::protocol::Manifest::new(
                        "st-sync-failure".into(),
                    ))
                    .unwrap(),
                )]),
            )
            .unwrap();
        assert!(transport.push(&root).unwrap().success);
        let client = Client::connect(transport.clone(), &td.path().join("cache")).unwrap();
        let hook = remote.join("hooks/pre-receive");
        fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        let request: Request = serde_json::from_value(serde_json::json!({
            "version":1,"request_id":"sync-failure","actor":"a","session":"s",
            "timestamp":"2026-10-20T00:00:00Z",
            "command":{"kind":"synthetic.cas","changes":[{"key":"task","expected":null,"value":"a"}]}
        })).unwrap();
        let first = client.submit(request.clone(), 2).unwrap();
        assert_eq!(first.status, SubmissionStatus::Unconfirmed);
        let retained = fs::read(client.cache.root().join("requests/sync-failure.json")).unwrap();
        fs::remove_file(hook).unwrap();
        let mut writes = 0;
        let result = client
            .submit_with_persistence(request, 2, &mut |entry| {
                writes += 1;
                assert_eq!(entry.attempted_commits.len(), 2);
                Err(std::io::Error::other("injected journal sync failure").into())
            })
            .unwrap();
        assert_eq!(
            writes, 1,
            "unchanged retained journal must not be rewritten"
        );
        assert_eq!(result.status, SubmissionStatus::Unconfirmed);
        assert_eq!(result.request_id, first.request_id);
        assert_eq!(result.attempted_commit, first.attempted_commit);
        assert_eq!(result.diagnostic.unwrap().cause, FailureCause::LocalState);
        assert_eq!(
            fs::read(client.cache.root().join("requests/sync-failure.json")).unwrap(),
            retained
        );
        assert_eq!(transport.head().unwrap(), Some(root));
        assert!(client.refresh().unwrap().receipts.is_empty());
    }
}
