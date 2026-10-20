//! Bounded Git object transport for the Git-authority protocol.
//!
//! This module deliberately keeps every mutable Git path below `scratch`. It
//! resolves the named remote's sole push URL, retains its supported transport
//! settings explicitly, and uses that same endpoint for reads and writes.
//! Consequently it does not change checkout refs, index, objects, or
//! `FETCH_HEAD`.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use super::{
    connection::RootArtifact,
    protocol::{Manifest, encode},
};
use crate::{MoteError, MoteResult};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitBlob {
    pub mode: String,
    pub oid: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCommit {
    pub oid: String,
    pub parents: Vec<String>,
    pub files: BTreeMap<String, GitBlob>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushFailure {
    Contention,
    Denied,
    Timeout,
    Unavailable,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushAttempt {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub failure: Option<PushFailure>,
}

// Diagnostics stay private: URLs and authentication helpers may write secrets.
// Only a bounded category is ever returned to the caller.
struct GitOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct GitTransport {
    repo: PathBuf,
    remote: String,
    reference: String,
    endpoint: String,
    upload_pack: Option<String>,
    receive_pack: Option<String>,
    proxy: Option<String>,
    proxy_auth_method: Option<String>,
    scratch: PathBuf,
    objects: PathBuf,
    source_objects: OsString,
    index: PathBuf,
    timeout: Duration,
    oid_len: usize,
    blob_reads: Arc<AtomicUsize>,
}

impl GitTransport {
    pub fn open(repo: &Path, remote: &str, branch: &str, scratch: &Path) -> MoteResult<Self> {
        validate_remote(remote)?;
        validate_branch(branch)?;
        let repo = repo
            .canonicalize()
            .map_err(|_| MoteError::Invalid("Git repository is unavailable".into()))?;
        let scratch = scratch.canonicalize().or_else(|_| {
            fs::create_dir_all(scratch)?;
            scratch.canonicalize()
        })?;
        if scratch.starts_with(&repo) || repo.starts_with(&scratch) {
            return Err(MoteError::Invalid(
                "Git transport scratch must be isolated from the source checkout".into(),
            ));
        }
        let objects = scratch.join("objects");
        fs::create_dir_all(&objects)?;
        let index = scratch.join("index");
        let source_objects = resolve_source_objects(&repo)?;

        let mut transport = Self {
            repo,
            remote: remote.into(),
            reference: format!("refs/heads/{branch}"),
            endpoint: String::new(),
            upload_pack: None,
            receive_pack: None,
            proxy: None,
            proxy_auth_method: None,
            scratch,
            objects,
            source_objects,
            index,
            timeout: DEFAULT_TIMEOUT,
            oid_len: 0,
            blob_reads: Arc::new(AtomicUsize::new(0)),
        };
        let object_format =
            transport.git_text("object format", ["rev-parse", "--show-object-format"])?;
        transport.oid_len = match object_format.trim() {
            "sha1" => 40,
            "sha256" => 64,
            _ => return Err(MoteError::Invalid("unsupported Git object format".into())),
        };
        let endpoints = transport.git_text(
            "remote endpoint",
            ["remote", "get-url", "--push", "--all", remote],
        )?;
        let endpoints: Vec<_> = endpoints.lines().filter(|line| !line.is_empty()).collect();
        if endpoints.len() != 1 {
            return Err(MoteError::Invalid(
                "Git authority requires exactly one push endpoint".into(),
            ));
        }
        transport.endpoint = endpoints[0].to_owned();
        transport.validate_remote_settings()?;
        transport.upload_pack = transport.remote_config("uploadpack")?;
        transport.receive_pack = transport.remote_config("receivepack")?;
        transport.proxy = transport.remote_config("proxy")?;
        transport.proxy_auth_method = transport.remote_config("proxyAuthMethod")?;
        Ok(transport)
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Stable identifier for the selected endpoint without exposing its URL.
    pub fn endpoint_digest(&self) -> String {
        blake3::hash(self.endpoint.as_bytes()).to_hex().to_string()
    }

    pub fn branch_ref(&self) -> &str {
        &self.reference
    }

    pub fn remote_name(&self) -> &str {
        &self.remote
    }

    /// Durable identity and journals must survive disposal of transport scratch.
    pub(crate) fn checked_cache_path(&self, path: &Path) -> MoteResult<PathBuf> {
        let mut ancestor = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let mut tail = Vec::new();
        while !ancestor.exists() {
            let name = ancestor
                .file_name()
                .ok_or_else(|| MoteError::Invalid("cache path has no existing ancestor".into()))?;
            tail.push(name.to_owned());
            ancestor.pop();
        }
        let mut resolved = ancestor.canonicalize()?;
        for name in tail.into_iter().rev() {
            resolved.push(name);
        }
        if resolved.starts_with(&self.scratch) || self.scratch.starts_with(&resolved) {
            return Err(MoteError::Invalid(
                "durable authority cache must be disjoint from disposable Git scratch".into(),
            ));
        }
        Ok(resolved)
    }

    /// Fetch the selected authority branch into scratch objects only.
    pub fn head(&self) -> MoteResult<Option<String>> {
        let mut list_args = vec!["ls-remote", "--refs"];
        if let Some(upload_pack) = &self.upload_pack {
            list_args.push("--upload-pack");
            list_args.push(upload_pack);
        }
        list_args.extend([self.remote.as_str(), self.reference.as_str()]);
        let listed = self.git_text("remote reference", list_args)?;
        let mut lines = listed.lines();
        let Some(line) = lines.next() else {
            return Ok(None);
        };
        if lines.next().is_some() {
            return Err(MoteError::Invalid(
                "remote returned duplicate authority reference".into(),
            ));
        }
        let mut fields = line.split_whitespace();
        let oid = fields
            .next()
            .ok_or_else(|| MoteError::Invalid("invalid remote reference response".into()))?;
        if fields.next() != Some(self.reference.as_str()) || fields.next().is_some() {
            return Err(MoteError::Invalid(
                "invalid remote reference response".into(),
            ));
        }
        self.validate_oid(oid)?;
        // `fetch` can update a configured remote-tracking ref even with an
        // empty refmap.  fetch-pack stores only the transferred objects.
        let mut fetch_args = vec!["fetch-pack", "--no-progress"];
        let upload_exec;
        if let Some(upload_pack) = &self.upload_pack {
            upload_exec = format!("--exec={upload_pack}");
            fetch_args.push(&upload_exec);
        }
        fetch_args.extend([self.remote.as_str(), self.reference.as_str()]);
        self.git_text("fetch authority objects", fetch_args)?;
        Ok(Some(oid.to_owned()))
    }

    pub fn commit(&self, oid: &str) -> MoteResult<GitCommit> {
        self.commit_memo(oid, &mut BTreeMap::new())
    }

    /// Per-load blob memo. Callers own its lifetime so validation always walks
    /// every commit and tree while identical immutable blobs are read once.
    pub(crate) fn commit_memo(
        &self,
        oid: &str,
        blobs: &mut BTreeMap<String, GitBlob>,
    ) -> MoteResult<GitCommit> {
        self.validate_oid(oid)?;
        let kind = self.git_text("read commit", ["cat-file", "-t", oid])?;
        if kind.trim() != "commit" {
            return Err(MoteError::Invalid("Git object is not a commit".into()));
        }
        let raw = self.git_bytes("read commit", ["cat-file", "commit", oid], None)?;
        let mut parents = Vec::new();
        for line in raw.split(|b| *b == b'\n') {
            if let Some(parent) = line.strip_prefix(b"parent ") {
                let parent = std::str::from_utf8(parent)
                    .map_err(|_| MoteError::Invalid("invalid commit parent".into()))?;
                self.validate_oid(parent)?;
                parents.push(parent.to_owned());
            }
            if line.is_empty() {
                break;
            }
        }
        let listing = self.git_bytes("list tree", ["ls-tree", "-r", "-z", oid], None)?;
        let mut files = BTreeMap::new();
        for entry in listing.split(|b| *b == 0).filter(|entry| !entry.is_empty()) {
            let tab = entry
                .iter()
                .position(|b| *b == b'\t')
                .ok_or_else(|| MoteError::Invalid("invalid tree entry".into()))?;
            let header = std::str::from_utf8(&entry[..tab])
                .map_err(|_| MoteError::Invalid("invalid tree entry".into()))?;
            let mut fields = header.split_whitespace();
            let mode = fields
                .next()
                .ok_or_else(|| MoteError::Invalid("invalid tree entry".into()))?;
            let kind = fields
                .next()
                .ok_or_else(|| MoteError::Invalid("invalid tree entry".into()))?;
            let blob_oid = fields
                .next()
                .ok_or_else(|| MoteError::Invalid("invalid tree entry".into()))?;
            if fields.next().is_some() || kind != "blob" {
                return Err(MoteError::Invalid(
                    "authority tree contains an unsupported entry".into(),
                ));
            }
            self.validate_oid(blob_oid)?;
            let path = std::str::from_utf8(&entry[tab + 1..])
                .map_err(|_| MoteError::Invalid("authority tree path is not UTF-8".into()))?;
            validate_tree_path(path)?;
            let blob = if let Some(blob) = blobs.get(blob_oid) {
                blob.clone()
            } else {
                let bytes = self.git_bytes("read blob", ["cat-file", "blob", blob_oid], None)?;
                self.blob_reads.fetch_add(1, Ordering::Relaxed);
                let blob = GitBlob {
                    mode: mode.to_owned(),
                    oid: blob_oid.to_owned(),
                    bytes,
                };
                blobs.insert(blob_oid.to_owned(), blob.clone());
                blob
            };
            if files
                .insert(
                    path.to_owned(),
                    GitBlob {
                        mode: mode.to_owned(),
                        ..blob
                    },
                )
                .is_some()
            {
                return Err(MoteError::Invalid("duplicate authority tree path".into()));
            }
        }
        Ok(GitCommit {
            oid: oid.into(),
            parents,
            files,
        })
    }

    pub fn create_commit(
        &self,
        parent: Option<&str>,
        files: &BTreeMap<String, Vec<u8>>,
    ) -> MoteResult<String> {
        if files.is_empty() {
            return Err(MoteError::Invalid(
                "Git authority commit requires at least one file".into(),
            ));
        }
        for path in files.keys() {
            validate_tree_path(path)?;
        }
        if let Some(parent) = parent {
            self.validate_oid(parent)?;
            if self
                .git_text("read parent", ["cat-file", "-t", parent])?
                .trim()
                != "commit"
            {
                return Err(MoteError::Invalid("Git parent is not a commit".into()));
            }
        }
        let _ = fs::remove_file(&self.index);
        if let Some(parent) = parent {
            self.git_text("load parent tree", ["read-tree", parent])?;
        }
        for (path, bytes) in files {
            let oid =
                self.git_bytes("write blob", ["hash-object", "-w", "--stdin"], Some(bytes))?;
            let oid = std::str::from_utf8(&oid)
                .map_err(|_| MoteError::Other("Git returned invalid object id".into()))?
                .trim();
            self.validate_oid(oid)?;
            self.git_text(
                "update tree",
                ["update-index", "--add", "--cacheinfo", "100644", oid, path],
            )?;
        }
        let tree = self.git_text("write tree", ["write-tree"])?;
        self.validate_oid(tree.trim())?;
        let mut args = vec!["-c", "commit.gpgsign=false", "commit-tree", tree.trim()];
        if let Some(parent) = parent {
            args.extend(["-p", parent]);
        }
        let oid = self.git_bytes(
            "create commit",
            args,
            Some(b"Mote Git authority transaction\n"),
        )?;
        let oid = std::str::from_utf8(&oid)
            .map_err(|_| MoteError::Other("Git returned invalid commit id".into()))?
            .trim();
        self.validate_oid(oid)?;
        Ok(oid.to_owned())
    }

    pub fn capture_root(&self, manifest: &Manifest) -> MoteResult<RootArtifact> {
        manifest.validate()?;
        let manifest_bytes = encode(manifest)?;
        let commit_oid = self.create_commit(
            None,
            &BTreeMap::from([("manifest.json".into(), manifest_bytes.clone())]),
        )?;
        let commit_bytes = self.git_bytes(
            "read root commit",
            ["cat-file", "commit", &commit_oid],
            None,
        )?;
        let tree_oid = self
            .git_text(
                "read root tree",
                ["rev-parse", &format!("{commit_oid}^{{tree}}")],
            )?
            .trim()
            .to_owned();
        self.validate_oid(&tree_oid)?;
        let tree_bytes = self.git_bytes("read root tree", ["cat-file", "tree", &tree_oid], None)?;
        let manifest_oid = self
            .git_text(
                "read root blob",
                ["rev-parse", &format!("{commit_oid}:manifest.json")],
            )?
            .trim()
            .to_owned();
        self.validate_oid(&manifest_oid)?;
        let artifact = RootArtifact {
            object_format: if self.oid_len == 40 { "sha1" } else { "sha256" }.into(),
            manifest_oid,
            manifest_bytes,
            tree_oid,
            tree_bytes,
            commit_oid,
            commit_bytes,
        };
        self.import_root(&artifact)?;
        Ok(artifact)
    }

    pub fn import_root(&self, artifact: &RootArtifact) -> MoteResult<()> {
        artifact.validate()?;
        if (artifact.object_format == "sha1") != (self.oid_len == 40) {
            return Err(MoteError::Invalid(
                "root artifact object format differs from transport".into(),
            ));
        }
        for (kind, oid, bytes) in [
            ("blob", &artifact.manifest_oid, &artifact.manifest_bytes),
            ("tree", &artifact.tree_oid, &artifact.tree_bytes),
            ("commit", &artifact.commit_oid, &artifact.commit_bytes),
        ] {
            let got = self.git_bytes(
                "import retained root object",
                ["hash-object", "-w", "-t", kind, "--stdin"],
                Some(bytes),
            )?;
            let got = std::str::from_utf8(&got)
                .map_err(|_| MoteError::Invalid("invalid imported object id".into()))?
                .trim();
            if got != oid {
                return Err(MoteError::Invalid(
                    "retained root object hash differs".into(),
                ));
            }
        }
        let commit = self.commit(&artifact.commit_oid)?;
        if !commit.parents.is_empty()
            || commit.files.len() != 1
            || commit.files.get("manifest.json").is_none_or(|b| {
                b.mode != "100644"
                    || b.oid != artifact.manifest_oid
                    || b.bytes != artifact.manifest_bytes
            })
        {
            return Err(MoteError::Invalid(
                "retained root artifact does not describe an authority root".into(),
            ));
        }
        let tree = self
            .git_text(
                "verify retained root tree",
                ["rev-parse", &format!("{}^{{tree}}", artifact.commit_oid)],
            )?
            .trim()
            .to_owned();
        if tree != artifact.tree_oid {
            return Err(MoteError::Invalid(
                "retained root tree differs from commit".into(),
            ));
        }
        Ok(())
    }

    /// Narrow append builder for an already-validated authority parent.
    pub(crate) fn append_commit(
        &self,
        parent: &str,
        path: &str,
        bytes: &[u8],
    ) -> MoteResult<String> {
        self.validate_oid(parent)?;
        validate_tree_path(path)?;
        if self
            .git_text("read parent", ["cat-file", "-t", parent])?
            .trim()
            != "commit"
        {
            return Err(MoteError::Invalid("Git parent is not a commit".into()));
        }
        if !self
            .git_bytes(
                "check append path",
                ["ls-tree", "-z", parent, "--", path],
                None,
            )?
            .is_empty()
        {
            return Err(MoteError::Invalid(
                "authority append path already exists".into(),
            ));
        }
        let _ = fs::remove_file(&self.index);
        self.git_text("load parent tree", ["read-tree", parent])?;
        let oid = self.git_bytes("write blob", ["hash-object", "-w", "--stdin"], Some(bytes))?;
        let oid = std::str::from_utf8(&oid)
            .map_err(|_| MoteError::Other("Git returned invalid object id".into()))?
            .trim();
        self.validate_oid(oid)?;
        self.git_text(
            "update tree",
            ["update-index", "--add", "--cacheinfo", "100644", oid, path],
        )?;
        let tree = self.git_text("write tree", ["write-tree"])?;
        self.validate_oid(tree.trim())?;
        let oid = self.git_bytes(
            "create commit",
            [
                "-c",
                "commit.gpgsign=false",
                "commit-tree",
                tree.trim(),
                "-p",
                parent,
            ],
            Some(b"Mote Git authority transaction\n"),
        )?;
        let oid = std::str::from_utf8(&oid)
            .map_err(|_| MoteError::Other("Git returned invalid object id".into()))?
            .trim();
        self.validate_oid(oid)?;
        Ok(oid.to_owned())
    }

    pub fn push(&self, oid: &str) -> MoteResult<PushAttempt> {
        self.validate_oid(oid)?;
        let refspec = format!("{oid}:{}", self.reference);
        let mut push_args = vec!["push", "--porcelain", "--no-follow-tags"];
        if let Some(receive_pack) = &self.receive_pack {
            push_args.push("--receive-pack");
            push_args.push(receive_pack);
        }
        push_args.extend([self.remote.as_str(), refspec.as_str()]);
        match self.run_git("push authority reference", push_args, None, false) {
            Ok(output) => Ok(PushAttempt {
                success: output.status.success(),
                exit_code: output.status.code(),
                failure: (!output.status.success())
                    .then(|| classify_push_failure(&output.stdout, &output.stderr)),
            }),
            Err(MoteError::Other(detail)) if detail.contains("timed out") => Ok(PushAttempt {
                success: false,
                exit_code: None,
                failure: Some(PushFailure::Timeout),
            }),
            Err(MoteError::Io(_)) => Ok(PushAttempt {
                success: false,
                exit_code: None,
                failure: Some(PushFailure::Unavailable),
            }),
            Err(error) => Err(error),
        }
    }

    fn validate_oid(&self, oid: &str) -> MoteResult<()> {
        if oid.len() != self.oid_len
            || !oid
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(MoteError::Invalid("invalid Git object id".into()));
        }
        Ok(())
    }

    fn git_text<'a>(
        &self,
        operation: &str,
        args: impl IntoIterator<Item = &'a str>,
    ) -> MoteResult<String> {
        let bytes = self.git_bytes(operation, args, None)?;
        String::from_utf8(bytes)
            .map_err(|_| MoteError::Other("Git returned non-text output".into()))
    }

    fn git_bytes<'a>(
        &self,
        operation: &str,
        args: impl IntoIterator<Item = &'a str>,
        input: Option<&[u8]>,
    ) -> MoteResult<Vec<u8>> {
        let output = self.run_git(operation, args, input, true)?;
        debug_assert!(output.status.success());
        Ok(output.stdout)
    }

    fn run_git<'a>(
        &self,
        operation: &str,
        args: impl IntoIterator<Item = &'a str>,
        input: Option<&[u8]>,
        checked: bool,
    ) -> MoteResult<GitOutput> {
        let marker = format!("{}-{}", std::process::id(), random_suffix());
        let stdout_path = self.scratch.join(format!("git-{marker}.stdout"));
        let stderr_path = self.scratch.join(format!("git-{marker}.stderr"));
        let stdin_path = self.scratch.join(format!("git-{marker}.stdin"));
        if let Some(input) = input {
            fs::write(&stdin_path, input)?;
        }
        let stdout = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stdout_path)?;
        let stderr = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stderr_path)?;
        restrict_to_owner(&stdout_path)?;
        restrict_to_owner(&stderr_path)?;
        if input.is_some() {
            restrict_to_owner(&stdin_path)?;
        }
        let args: Vec<_> = args
            .into_iter()
            .map(|arg| {
                if !self.endpoint.is_empty() && arg == self.remote {
                    self.endpoint.as_str()
                } else {
                    arg
                }
            })
            .collect();
        let mut command = Command::new("git");
        command.current_dir(&self.repo);
        if let Some(proxy) = &self.proxy {
            command.args(["-c", &format!("http.proxy={proxy}")]);
        }
        if let Some(method) = &self.proxy_auth_method {
            command.args(["-c", &format!("http.proxyAuthMethod={method}")]);
        }
        command
            .args(&args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .env("GIT_OBJECT_DIRECTORY", &self.objects)
            .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &self.source_objects)
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_INDEX_FILE", &self.index)
            .env("GIT_AUTHOR_NAME", "Mote Git Authority")
            .env("GIT_AUTHOR_EMAIL", "git-authority@mote.invalid")
            .env("GIT_COMMITTER_NAME", "Mote Git Authority")
            .env("GIT_COMMITTER_EMAIL", "git-authority@mote.invalid")
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        if input.is_some() {
            command.stdin(Stdio::from(File::open(&stdin_path)?));
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn()?;
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                #[cfg(unix)]
                {
                    let group = format!("-{}", child.id());
                    let _ = Command::new("kill")
                        .args(["-TERM", "--", &group])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                }
                let _ = child.kill();
                let _ = child.wait();
                cleanup(&[&stdout_path, &stderr_path, &stdin_path]);
                return Err(MoteError::Other(format!(
                    "Git {operation} timed out; a submitted push may have committed"
                )));
            }
            thread::sleep(Duration::from_millis(10));
        };
        let bytes =
            (|| -> std::io::Result<_> { Ok((fs::read(&stdout_path)?, fs::read(&stderr_path)?)) })();
        cleanup(&[&stdout_path, &stderr_path, &stdin_path]);
        let (stdout, stderr) = bytes?;
        if checked && !status.success() {
            return Err(MoteError::Rejected(format!(
                "Git {operation} failed (exit {})",
                status
                    .code()
                    .map_or_else(|| "signal".into(), |n| n.to_string())
            )));
        }
        Ok(GitOutput {
            status,
            stdout,
            stderr,
        })
    }
}

fn resolve_source_objects(repo: &Path) -> MoteResult<OsString> {
    let output = Command::new("git")
        .current_dir(repo)
        .args(["--no-replace-objects", "rev-parse", "--git-path", "objects"])
        .output()?;
    if !output.status.success() {
        return Err(MoteError::Invalid("Git repository is unavailable".into()));
    }
    let raw = String::from_utf8(output.stdout)
        .map_err(|_| MoteError::Invalid("invalid Git object directory".into()))?;
    let objects = repo
        .join(raw.trim())
        .canonicalize()
        .map_err(|_| MoteError::Invalid("Git object directory is unavailable".into()))?;
    Ok(objects.into_os_string())
}

impl GitTransport {
    fn remote_config(&self, name: &str) -> MoteResult<Option<String>> {
        let key = format!("remote.{}.{}", self.remote, name);
        let output = Command::new("git")
            .current_dir(&self.repo)
            .args(["config", "--get", &key])
            .output()?;
        if output.status.code() == Some(1) {
            return Ok(None);
        }
        if !output.status.success() {
            return Err(MoteError::Invalid(
                "unable to read Git remote configuration".into(),
            ));
        }
        let value = String::from_utf8(output.stdout)
            .map_err(|_| MoteError::Invalid("invalid Git remote configuration".into()))?;
        let value = value.trim_end_matches(['\r', '\n']);
        if value.is_empty()
            || value
                .bytes()
                .any(|byte| byte == 0 || byte.is_ascii_control())
        {
            return Err(MoteError::Invalid(
                "unsupported Git remote configuration".into(),
            ));
        }
        Ok(Some(value.into()))
    }

    fn validate_remote_settings(&self) -> MoteResult<()> {
        let prefix = format!("remote.{}.", self.remote).to_ascii_lowercase();
        let pattern = format!("^remote\\.{}\\.", self.remote);
        let output = Command::new("git")
            .current_dir(&self.repo)
            .args(["config", "--null", "--get-regexp", &pattern])
            .output()?;
        if output.status.code() == Some(1) {
            return Ok(());
        }
        if !output.status.success() {
            return Err(MoteError::Invalid(
                "unable to read Git remote configuration".into(),
            ));
        }
        for entry in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
        {
            let Some(separator) = entry.iter().position(|byte| *byte == b'\n') else {
                return Err(MoteError::Invalid(
                    "invalid Git remote configuration".into(),
                ));
            };
            let key = std::str::from_utf8(&entry[..separator])
                .map_err(|_| MoteError::Invalid("invalid Git remote configuration".into()))?
                .to_ascii_lowercase();
            let name = key
                .strip_prefix(&prefix)
                .ok_or_else(|| MoteError::Invalid("invalid Git remote configuration".into()))?;
            if !matches!(
                name,
                "url"
                    | "pushurl"
                    | "fetch"
                    | "proxy"
                    | "proxyauthmethod"
                    | "uploadpack"
                    | "receivepack"
            ) {
                return Err(MoteError::Invalid(
                    "unsupported Git remote-specific setting".into(),
                ));
            }
        }
        Ok(())
    }
}

fn classify_push_failure(porcelain: &[u8], diagnostics: &[u8]) -> PushFailure {
    let text = String::from_utf8_lossy(porcelain).to_ascii_lowercase();
    let diagnostic = String::from_utf8_lossy(diagnostics).to_ascii_lowercase();
    if (text.contains("[remote rejected]") && text.contains("failed to update ref"))
        || text.contains("[rejected]")
    {
        PushFailure::Contention
    } else if text.contains("[remote rejected]")
        || text.contains("hook declined")
        || [
            "permission denied",
            "authentication failed",
            "access denied",
            "could not read username",
            "requested url returned error: 401",
            "requested url returned error: 403",
            "authorization failed",
        ]
        .iter()
        .any(|pattern| diagnostic.contains(pattern))
    {
        PushFailure::Denied
    } else if ["connection timed out", "operation timed out"]
        .iter()
        .any(|pattern| diagnostic.contains(pattern))
    {
        PushFailure::Timeout
    } else if [
        "could not resolve host",
        "could not resolve hostname",
        "connection refused",
        "network is unreachable",
        "connection reset",
        "connection closed",
        "does not appear to be a git repository",
        "ssl certificate problem",
    ]
    .iter()
    .any(|pattern| diagnostic.contains(pattern))
    {
        PushFailure::Unavailable
    } else {
        PushFailure::Unknown
    }
}

fn cleanup(paths: &[&Path]) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> MoteResult<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) -> MoteResult<()> {
    Ok(())
}

fn random_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn validate_remote(remote: &str) -> MoteResult<()> {
    if remote.is_empty()
        || !remote
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(MoteError::Invalid("invalid Git remote name".into()));
    }
    Ok(())
}

fn validate_branch(branch: &str) -> MoteResult<()> {
    if branch.is_empty()
        || branch.starts_with('-')
        || branch.starts_with('/')
        || branch.ends_with('/')
        || branch.contains("..")
        || branch.contains("@{")
        || branch.bytes().any(|b| {
            b <= b' '
                || b == b'~'
                || b == b'^'
                || b == b':'
                || b == b'?'
                || b == b'*'
                || b == b'['
                || b == b'\\'
        })
    {
        return Err(MoteError::Invalid("invalid Git authority branch".into()));
    }
    Ok(())
}

fn validate_tree_path(path: &str) -> MoteResult<()> {
    if path.is_empty()
        || path.starts_with('/')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || path.as_bytes().contains(&0)
    {
        return Err(MoteError::Invalid("invalid Git tree path".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_paths_cannot_overlap_scratch_even_through_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        git(root.path(), &["init", "-q", repo.to_str().unwrap()]);
        git(&repo, &["remote", "add", "origin", "unused"]);
        let scratch = root.path().join("scratch");
        let transport = GitTransport::open(&repo, "origin", "authority", &scratch).unwrap();
        for cache in [
            &scratch,
            &scratch.join("durable"),
            &root.path().to_path_buf(),
        ] {
            assert!(transport.checked_cache_path(cache).is_err());
        }
        #[cfg(unix)]
        {
            let alias = root.path().join("alias");
            std::os::unix::fs::symlink(&scratch, &alias).unwrap();
            assert!(transport.checked_cache_path(&alias.join("nested")).is_err());
        }
        let durable = root.path().join("durable");
        assert_eq!(
            transport.checked_cache_path(&durable).unwrap(),
            root.path().canonicalize().unwrap().join("durable")
        );
        assert!(!durable.exists(), "validation must not create cache state");
    }

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
    fn blob_memo_reads_each_oid_once_and_cold_reads_match() {
        let root = tempfile::TempDir::new().unwrap();
        let repo = root.path().join("repo");
        let remote = root.path().join("remote.git");
        git(root.path(), &["init", repo.to_str().unwrap()]);
        git(root.path(), &["init", "--bare", remote.to_str().unwrap()]);
        git(
            &repo,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        let transport =
            GitTransport::open(&repo, "origin", "authority", &root.path().join("scratch")).unwrap();
        let first = transport
            .create_commit(
                None,
                &BTreeMap::from([("manifest.json".into(), b"same".to_vec())]),
            )
            .unwrap();
        let second = transport
            .append_commit(&first, "transactions/0001.json", b"next")
            .unwrap();
        transport.blob_reads.store(0, Ordering::Relaxed);
        let cold_first = transport.commit(&first).unwrap();
        let cold_second = transport.commit(&second).unwrap();
        let cold_reads = transport.blob_reads.load(Ordering::Relaxed);
        transport.blob_reads.store(0, Ordering::Relaxed);
        let mut memo = BTreeMap::new();
        let memo_second = transport.commit_memo(&second, &mut memo).unwrap();
        let memo_first = transport.commit_memo(&first, &mut memo).unwrap();
        assert_eq!(memo_second, cold_second);
        assert_eq!(memo_first, cold_first);
        assert_eq!(transport.blob_reads.load(Ordering::Relaxed), 2);
        assert_eq!(cold_reads, 3);
    }

    #[test]
    fn narrow_append_preserves_all_parent_entries_and_refuses_reuse() {
        let root = tempfile::TempDir::new().unwrap();
        let repo = root.path().join("repo");
        let remote = root.path().join("remote.git");
        git(root.path(), &["init", repo.to_str().unwrap()]);
        git(root.path(), &["init", "--bare", remote.to_str().unwrap()]);
        git(
            &repo,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        let transport =
            GitTransport::open(&repo, "origin", "authority", &root.path().join("scratch")).unwrap();
        let files = BTreeMap::from([
            ("manifest.json".into(), b"manifest".to_vec()),
            ("transactions/0001.json".into(), b"old".to_vec()),
        ]);
        let parent = transport.create_commit(None, &files).unwrap();
        let before = transport.commit(&parent).unwrap();
        let child = transport
            .append_commit(&parent, "transactions/0002.json", b"new")
            .unwrap();
        let after = transport.commit(&child).unwrap();
        assert_eq!(after.parents, [parent]);
        assert_eq!(after.files.len(), before.files.len() + 1);
        for (path, blob) in before.files {
            assert_eq!(after.files[&path], blob);
        }
        assert_eq!(after.files["transactions/0002.json"].mode, "100644");
        assert_eq!(after.files["transactions/0002.json"].bytes, b"new");
        assert!(
            transport
                .append_commit(&child, "transactions/0001.json", b"replace")
                .is_err()
        );
    }

    #[test]
    fn memoized_blob_content_does_not_hide_each_tree_entry_mode() {
        use crate::git_authority::{
            history::validate_chain,
            protocol::{Manifest, encode, transaction_path},
        };
        let root = tempfile::TempDir::new().unwrap();
        let repo = root.path().join("repo");
        let remote = root.path().join("remote.git");
        git(root.path(), &["init", repo.to_str().unwrap()]);
        git(root.path(), &["init", "--bare", remote.to_str().unwrap()]);
        git(
            &repo,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        let transport =
            GitTransport::open(&repo, "origin", "authority", &root.path().join("scratch")).unwrap();
        let parent = transport
            .create_commit(
                None,
                &BTreeMap::from([(
                    "manifest.json".into(),
                    encode(&Manifest::new("st-test".into())).unwrap(),
                )]),
            )
            .unwrap();
        let before = transport.commit(&parent).unwrap();
        let path = transaction_path(1);
        transport
            .append_commit(&parent, &path, b"invalid transaction")
            .unwrap();
        transport
            .git_text(
                "change test mode",
                [
                    "update-index",
                    "--cacheinfo",
                    "100755",
                    &before.files["manifest.json"].oid,
                    "manifest.json",
                ],
            )
            .unwrap();
        let tree = transport.git_text("test tree", ["write-tree"]).unwrap();
        let child = transport
            .git_text("test commit", ["commit-tree", tree.trim(), "-p", &parent])
            .unwrap();
        let child = child.trim();
        let mut memo = BTreeMap::new();
        let after = transport.commit_memo(child, &mut memo).unwrap();
        let before_memo = transport.commit_memo(&parent, &mut memo).unwrap();
        assert_eq!(after.files["manifest.json"].mode, "100755");
        assert_eq!(before_memo.files["manifest.json"].mode, "100644");
        assert_eq!(before_memo, before);
        let error = validate_chain(&[before_memo, after], None, None).unwrap_err();
        assert!(
            error.to_string().contains("append changed history"),
            "{error}"
        );
    }
}
