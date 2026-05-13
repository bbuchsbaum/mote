//! Clap definitions and dispatcher.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use jiff::Timestamp;

use crate::errors::{MoteError, MoteResult};
use crate::ids;
use crate::op::{
    self, ScalarSet, Status, make_board_post, make_board_read, make_board_sticky, make_board_topic,
    make_claim, make_close, make_create, make_delete, make_dep, make_msg_ack, make_msg_send,
    make_note, make_patch, make_release, make_reserve_close, make_reserve_open, make_tag,
    validate_msg_kind, validate_note_kind,
};
use crate::reducer;
use crate::state::Bead;
use crate::{fsck, publish, repo::Store};

#[derive(Parser, Debug)]
#[command(
    name = "mote",
    version,
    about = "Immutable op-maildir issue + coordination tracker"
)]
pub struct Cli {
    /// Override actor identity for this invocation
    #[arg(long, global = true)]
    pub actor: Option<String>,

    /// Override store path (default: walk up from cwd looking for .mote/)
    #[arg(long, global = true)]
    pub store: Option<PathBuf>,

    /// Machine-readable output where applicable
    #[arg(long, global = true)]
    pub json: bool,

    /// Suppress non-essential stderr
    #[arg(long, global = true)]
    pub quiet: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Initialize a `.mote/` store in the current directory
    Init,

    /// Manage the local actor identity in `.mote/local/actor`
    Actor {
        #[command(subcommand)]
        cmd: ActorCmd,
    },

    /// Create a new bead
    New {
        /// Title (required, non-empty)
        title: String,
        /// Priority 0..=3 (0 = highest)
        #[arg(short = 'p', long)]
        priority: Option<i32>,
        /// Initial body
        #[arg(long)]
        body: Option<String>,
        /// Initial assignee
        #[arg(long)]
        assignee: Option<String>,
        /// Tags to add (repeatable)
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Parent dependencies to add (repeatable; relationship `blocks`)
        #[arg(long = "dep")]
        deps: Vec<String>,
        /// Use this exact bead id instead of minting a `bd-...` ulid.
        /// Intended for migrations from another tracker. Reserved prefix
        /// `bd-` is rejected; collisions are caught by the reducer.
        #[arg(long)]
        id: Option<String>,
    },

    /// Update scalar fields on a bead via a single patch op.
    /// Accepts `field=value` pairs: title, status, priority, body, assignee.
    Set {
        id: String,
        #[arg(num_args = 1.., required = true)]
        fields: Vec<String>,
    },

    /// Show full state of a bead
    Show { id: String },

    /// List beads
    Ls {
        /// Filter by status (open|doing|blocked|review|closed)
        #[arg(long)]
        status: Option<String>,
        /// Filter by tag
        #[arg(long)]
        tag: Option<String>,
        /// Filter by assignee
        #[arg(long)]
        assignee: Option<String>,
        /// Include closed beads (off by default)
        #[arg(long)]
        all: bool,
        /// Show only ready beads (alias for `mote ready`).
        /// Combines as an additional filter on top of --status / --tag / --assignee.
        #[arg(long)]
        ready: bool,
    },

    /// List ready beads (open + no open blockers)
    Ready,

    /// Append a note to a bead
    Note {
        id: String,
        /// One of: note | progress | decision | handoff | blocker
        #[arg(long = "kind")]
        note_kind: String,
        text: String,
    },

    /// Print accepted (and optionally rejected) history of a bead
    History {
        id: String,
        #[arg(long)]
        include_rejected: bool,
    },

    /// Manage dependencies
    Dep {
        #[command(subcommand)]
        cmd: DepCmd,
    },

    /// Manage tags
    Tag {
        #[command(subcommand)]
        cmd: TagCmd,
    },

    /// Set status=closed (idempotent)
    Close { id: String },

    /// Tombstone a bead
    Delete { id: String },

    /// Claim a bead with a TTL-bounded lease
    Claim {
        id: String,
        /// TTL in seconds (defaults to FORMAT.json default_ttl_s.claim)
        #[arg(long)]
        ttl: Option<u32>,
    },

    /// Release the current claim on a bead
    Release { id: String },

    /// Direct message commands
    Msg {
        #[command(subcommand)]
        cmd: MsgCmd,
    },

    /// Public message-board discussion commands
    Discuss {
        #[command(subcommand)]
        cmd: DiscussCmd,
    },

    /// List unacked messages addressed to me
    Inbox {
        /// Filter by issue id
        #[arg(long)]
        issue: Option<String>,
        /// Filter by sender actor
        #[arg(long)]
        from: Option<String>,
        /// Filter by msg_kind
        #[arg(long)]
        kind: Option<String>,
    },

    /// Reserve one or more repo-relative paths under an issue
    Reserve {
        /// Repo-relative paths (file or directory; trailing `/` = directory prefix)
        #[arg(num_args = 1..)]
        paths: Vec<String>,
        /// Issue (bead) the reservation is for
        #[arg(long = "issue")]
        issue: String,
        /// TTL in seconds (default: FORMAT.json default_ttl_s.reservation)
        #[arg(long)]
        ttl: Option<u32>,
    },

    /// Close a reservation (or specific paths within it)
    Unreserve {
        /// Reservation id (rv-...)
        rv: String,
        /// Optional path subset to close (default: all)
        #[arg(long = "paths", num_args = 1..)]
        paths: Vec<String>,
    },

    /// Dry-run: report any reservation overlaps for given paths
    Preflight {
        /// Issue context
        #[arg(long = "issue")]
        issue: String,
        #[arg(long = "paths", num_args = 1..)]
        paths: Vec<String>,
    },

    /// Compound: reserve_open + claim + optional progress note. Compensates on partial failure.
    Begin {
        id: String,
        #[arg(long = "paths", num_args = 1..)]
        paths: Vec<String>,
        #[arg(long)]
        note: Option<String>,
        #[arg(long)]
        ttl: Option<u32>,
    },

    /// Compound: handoff note + claim transfer + optional reservation release
    Handoff {
        id: String,
        #[arg(long = "to")]
        to: String,
        #[arg(long)]
        note: Option<String>,
        /// Also close any current actor's reservations on this issue
        #[arg(long)]
        release: bool,
    },

    /// Compound: completion note + close + reserve_close + release
    Done {
        id: String,
        #[arg(long)]
        note: Option<String>,
    },

    /// Show live reservations whose paths overlap a given path
    WhoHas { path: String },

    /// Compact overview of board state
    Board,

    /// Check store layout, actor identity, and op-log health
    Doctor,

    /// Verify op-file hashes; with --clean-tmp, remove stale tmp/ entries
    Fsck {
        #[arg(long)]
        clean_tmp: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ActorCmd {
    /// Persist an actor identity in `.mote/local/actor`
    Set { actor: String },
    /// Show the actor identity that would be used for this invocation
    Show,
    /// Remove the persisted local actor identity
    Clear,
}

#[derive(Subcommand, Debug)]
pub enum DepCmd {
    /// Add a dependency edge: `child` is blocked by `parent`
    Add {
        child: String,
        parent: String,
        #[arg(long, default_value = "blocks")]
        kind: String,
    },
    /// Remove a dependency edge
    Rm {
        child: String,
        parent: String,
        #[arg(long, default_value = "blocks")]
        kind: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum TagCmd {
    Add { id: String, tag: String },
    Rm { id: String, tag: String },
}

#[derive(Subcommand, Debug)]
pub enum MsgCmd {
    /// Send a direct message
    Send {
        /// Recipient actor
        #[arg(long)]
        to: String,
        /// Optional issue context
        #[arg(long)]
        issue: Option<String>,
        /// Optional reservation context
        #[arg(long)]
        reservation: Option<String>,
        /// Message kind (note | request | handoff | blocked | fyi)
        #[arg(long = "kind", default_value = "note")]
        msg_kind: String,
        /// Body text (positional)
        text: String,
    },
    /// Acknowledge receipt of a message
    Ack {
        /// msg-id to ack
        msg_id: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum DiscussCmd {
    /// Add a public post to the message board
    Post {
        /// Topic name (default: general)
        #[arg(long, default_value = "general")]
        topic: String,
        /// Optional parent post id for a threaded reply
        #[arg(long = "reply-to")]
        reply_to: Option<String>,
        /// Body text (positional)
        text: String,
    },
    /// List public posts
    List {
        /// Filter by topic
        #[arg(long)]
        topic: Option<String>,
        /// Maximum number of posts to print
        #[arg(long)]
        limit: Option<usize>,
    },
    /// List posts newer than this actor's discussion read cursor
    Unread {
        /// Filter by topic
        #[arg(long)]
        topic: Option<String>,
        /// Maximum number of posts to print
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Mark currently visible discussion posts as read for this actor
    MarkRead {
        /// Mark only one topic read
        #[arg(long)]
        topic: Option<String>,
    },
    /// List direct replies to a post
    Replies {
        /// Parent post id
        post_id: String,
    },
    /// Show a post with all descendant replies
    Thread {
        /// Root post id
        post_id: String,
    },
    /// Manage discussion topics
    Topic {
        #[command(subcommand)]
        cmd: DiscussTopicCmd,
    },
    /// Search topics and posts
    Search {
        query: String,
        /// Filter posts by topic
        #[arg(long)]
        topic: Option<String>,
        /// Maximum number of topic matches and post matches to print
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Mark a post sticky
    Sticky { post_id: String },
    /// Remove sticky state from a post
    Unsticky { post_id: String },
    /// List topics with post counts
    Topics,
}

#[derive(Subcommand, Debug)]
pub enum DiscussTopicCmd {
    /// Create a topic before any posts exist
    New {
        topic: String,
        /// Display title (defaults to topic)
        #[arg(long)]
        title: Option<String>,
        /// Optional topic description
        #[arg(long)]
        body: Option<String>,
    },
}

pub fn run(cli: Cli) -> MoteResult<i32> {
    match cli.command {
        Command::Init => cmd_init(cli.quiet),
        Command::Actor { cmd } => {
            cmd_actor(cli.actor.as_deref(), cli.store.as_deref(), cli.json, cmd)
        }
        Command::New {
            title,
            priority,
            body,
            assignee,
            tags,
            deps,
            id,
        } => cmd_new(
            cli.actor.as_deref(),
            cli.store.as_deref(),
            cli.json,
            title,
            priority,
            body,
            assignee,
            tags,
            deps,
            id,
        ),
        Command::Set { id, fields } => cmd_set(
            cli.actor.as_deref(),
            cli.store.as_deref(),
            cli.json,
            id,
            fields,
        ),
        Command::Show { id } => cmd_show(cli.store.as_deref(), cli.json, id),
        Command::Ls {
            status,
            tag,
            assignee,
            all,
            ready,
        } => cmd_ls(
            cli.actor.as_deref(),
            cli.store.as_deref(),
            cli.json,
            status,
            tag,
            assignee,
            all,
            ready,
        ),
        Command::Ready => cmd_ready(cli.actor.as_deref(), cli.store.as_deref(), cli.json),
        Command::Note {
            id,
            note_kind,
            text,
        } => cmd_note(
            cli.actor.as_deref(),
            cli.store.as_deref(),
            id,
            note_kind,
            text,
        ),
        Command::History {
            id,
            include_rejected,
        } => cmd_history(cli.store.as_deref(), cli.json, id, include_rejected),
        Command::Dep { cmd } => cmd_dep(cli.actor.as_deref(), cli.store.as_deref(), cmd),
        Command::Tag { cmd } => cmd_tag(cli.actor.as_deref(), cli.store.as_deref(), cmd),
        Command::Close { id } => cmd_close(cli.actor.as_deref(), cli.store.as_deref(), id),
        Command::Delete { id } => cmd_delete(cli.actor.as_deref(), cli.store.as_deref(), id),
        Command::Claim { id, ttl } => {
            cmd_claim(cli.actor.as_deref(), cli.store.as_deref(), id, ttl)
        }
        Command::Release { id } => cmd_release(cli.actor.as_deref(), cli.store.as_deref(), id),
        Command::Msg { cmd } => cmd_msg(cli.actor.as_deref(), cli.store.as_deref(), cmd),
        Command::Discuss { cmd } => {
            cmd_discuss(cli.actor.as_deref(), cli.store.as_deref(), cli.json, cmd)
        }
        Command::Inbox { issue, from, kind } => cmd_inbox(
            cli.actor.as_deref(),
            cli.store.as_deref(),
            cli.json,
            issue,
            from,
            kind,
        ),
        Command::Reserve { paths, issue, ttl } => cmd_reserve(
            cli.actor.as_deref(),
            cli.store.as_deref(),
            paths,
            issue,
            ttl,
        ),
        Command::Unreserve { rv, paths } => {
            cmd_unreserve(cli.actor.as_deref(), cli.store.as_deref(), rv, paths)
        }
        Command::Preflight { issue, paths } => cmd_preflight(
            cli.actor.as_deref(),
            cli.store.as_deref(),
            cli.json,
            issue,
            paths,
        ),
        Command::Begin {
            id,
            paths,
            note,
            ttl,
        } => cmd_begin(
            cli.actor.as_deref(),
            cli.store.as_deref(),
            id,
            paths,
            note,
            ttl,
        ),
        Command::Handoff {
            id,
            to,
            note,
            release,
        } => cmd_handoff(
            cli.actor.as_deref(),
            cli.store.as_deref(),
            id,
            to,
            note,
            release,
        ),
        Command::Done { id, note } => {
            cmd_done(cli.actor.as_deref(), cli.store.as_deref(), id, note)
        }
        Command::WhoHas { path } => cmd_who_has(cli.store.as_deref(), cli.json, path),
        Command::Board => cmd_board(cli.actor.as_deref(), cli.store.as_deref(), cli.json),
        Command::Doctor => cmd_doctor(cli.actor.as_deref(), cli.store.as_deref(), cli.json),
        Command::Fsck { clean_tmp } => cmd_fsck(cli.store.as_deref(), cli.json, clean_tmp),
    }
}

fn cmd_init(quiet: bool) -> MoteResult<i32> {
    let cwd = std::env::current_dir()?;
    match Store::init(&cwd) {
        Ok(store) => {
            if !quiet {
                println!("initialized store at {}", store.root().display());
            }
        }
        Err(MoteError::StoreAlreadyInitialized(root)) => {
            // Idempotent per PRD: already-initialized is success.
            if !quiet {
                println!("store already initialized at {}", root.display());
            }
        }
        Err(e) => return Err(e),
    }
    Ok(0)
}

fn cmd_actor(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    json_mode: bool,
    cmd: ActorCmd,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor_path = store.local_dir().join("actor");

    match cmd {
        ActorCmd::Set { actor } => {
            let actor = normalize_actor(&actor)?;
            fs::create_dir_all(store.local_dir())?;
            fs::write(&actor_path, format!("{actor}\n"))?;

            if json_mode {
                let v = serde_json::json!({
                    "actor": actor,
                    "source": "local",
                    "path": actor_path.display().to_string(),
                });
                println!("{}", serde_json::to_string(&v)?);
            } else {
                println!("{actor}");
            }
            Ok(0)
        }
        ActorCmd::Show => {
            let resolved = resolve_actor_with_source(&store, actor_flag)?;
            if json_mode {
                let v = serde_json::json!({
                    "actor": resolved.actor,
                    "source": resolved.source,
                });
                println!("{}", serde_json::to_string(&v)?);
            } else {
                println!("{} ({})", resolved.actor, resolved.source);
            }
            Ok(0)
        }
        ActorCmd::Clear => {
            match fs::remove_file(&actor_path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            if json_mode {
                let v = serde_json::json!({
                    "cleared": true,
                    "path": actor_path.display().to_string(),
                });
                println!("{}", serde_json::to_string(&v)?);
            } else {
                println!("cleared {}", actor_path.display());
            }
            Ok(0)
        }
    }
}

fn normalize_actor(actor: &str) -> MoteResult<String> {
    let actor = actor.trim();
    if actor.is_empty() {
        return Err(MoteError::Invalid("actor must be non-empty".into()));
    }
    if actor.chars().any(|c| c == '\0' || c == '\n' || c == '\r') {
        return Err(MoteError::Invalid(
            "actor must be a single-line string".into(),
        ));
    }
    Ok(actor.to_string())
}

struct ActorResolution {
    actor: String,
    source: &'static str,
}

fn resolve_actor_with_source(
    store: &Store,
    actor_flag: Option<&str>,
) -> MoteResult<ActorResolution> {
    if let Some(s) = actor_flag {
        let s = s.trim();
        if !s.is_empty() {
            return Ok(ActorResolution {
                actor: s.to_string(),
                source: "flag",
            });
        }
    }
    if let Ok(s) = std::env::var("MOTE_ACTOR") {
        let s = s.trim();
        if !s.is_empty() {
            return Ok(ActorResolution {
                actor: s.to_string(),
                source: "env",
            });
        }
    }
    let actor_file = store.local_dir().join("actor");
    if actor_file.is_file() {
        let s = fs::read_to_string(&actor_file)?;
        let s = s.trim();
        if !s.is_empty() {
            return Ok(ActorResolution {
                actor: s.to_string(),
                source: "local",
            });
        }
    }
    Err(MoteError::ActorUnresolved)
}

#[allow(clippy::too_many_arguments)]
fn cmd_new(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    _json: bool,
    title: String,
    priority: Option<i32>,
    body: Option<String>,
    assignee: Option<String>,
    tags: Vec<String>,
    deps: Vec<String>,
    id: Option<String>,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;

    if title.trim().is_empty() {
        return Err(MoteError::Invalid("title must be non-empty".into()));
    }
    if let Some(p) = priority {
        if !(0..=3).contains(&p) {
            return Err(MoteError::Invalid(format!("priority {p} out of 0..=3")));
        }
    }

    let bead_id = match id {
        Some(custom) => {
            ids::validate_external_bead_id(&custom)?;
            custom
        }
        None => ids::new_bead_id(),
    };
    let mut set = ScalarSet {
        title: Some(title),
        priority,
        body,
        assignee,
        ..Default::default()
    };
    set.status = Some(Status::Open);

    let create = make_create(actor.clone(), bead_id.clone(), set, Timestamp::now());
    let create_name = publish::publish_op(&store, &create)?;

    let mut tag_names = Vec::new();
    for t in &tags {
        let op = make_tag(
            true,
            actor.clone(),
            bead_id.clone(),
            t.clone(),
            Timestamp::now(),
        );
        tag_names.push(publish::publish_op(&store, &op)?);
    }
    let mut dep_names = Vec::new();
    for d in &deps {
        let op = make_dep(
            true,
            actor.clone(),
            bead_id.clone(),
            d.clone(),
            "blocks".into(),
            Timestamp::now(),
        );
        dep_names.push(publish::publish_op(&store, &op)?);
    }

    // Verify acceptance.
    let state = reducer::replay_store(&store)?;
    let mut had_failure = false;
    if !state.was_accepted(create_name.as_str()) {
        had_failure = true;
        let reason = state
            .rejection_reason(create_name.as_str())
            .unwrap_or_else(|| "unknown".into());
        eprintln!("create rejected: {reason}");
    }
    for n in tag_names.iter().chain(dep_names.iter()) {
        if !state.was_accepted(n.as_str()) {
            had_failure = true;
            let reason = state
                .rejection_reason(n.as_str())
                .unwrap_or_else(|| "unknown".into());
            eprintln!("{} rejected: {reason}", n.as_str());
        }
    }

    println!("{bead_id}");
    Ok(if had_failure { 2 } else { 0 })
}

fn cmd_set(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    _json: bool,
    id: String,
    fields: Vec<String>,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;
    let state = reducer::replay_store(&store)?;
    let bead = state
        .beads
        .get(&id)
        .ok_or_else(|| MoteError::Invalid(format!("no such bead {id}")))?;
    if bead.is_deleted() {
        return Err(MoteError::Invalid(format!("bead {id} is deleted")));
    }

    let mut set = ScalarSet::default();
    let mut expect: BTreeMap<String, String> = BTreeMap::new();
    for kv in fields {
        let (key, value) = kv
            .split_once('=')
            .ok_or_else(|| MoteError::Invalid(format!("expected field=value, got {kv}")))?;
        match key {
            "title" => {
                set.title = Some(value.to_string());
                expect.insert("title".into(), clock_for(bead, "title")?);
            }
            "status" => {
                let s = Status::parse(value)
                    .ok_or_else(|| MoteError::Invalid(format!("invalid status: {value}")))?;
                set.status = Some(s);
                expect.insert("status".into(), clock_for(bead, "status")?);
            }
            "priority" => {
                let p: i32 = value
                    .parse()
                    .map_err(|_| MoteError::Invalid(format!("invalid priority: {value}")))?;
                if !(0..=3).contains(&p) {
                    return Err(MoteError::Invalid(format!("priority {p} out of 0..=3")));
                }
                set.priority = Some(p);
                expect.insert("priority".into(), clock_for(bead, "priority")?);
            }
            "body" => {
                set.body = Some(value.to_string());
                expect.insert("body".into(), clock_for(bead, "body")?);
            }
            "assignee" => {
                set.assignee = Some(value.to_string());
                if let Ok(c) = clock_for(bead, "assignee") {
                    expect.insert("assignee".into(), c);
                }
            }
            other => {
                return Err(MoteError::Invalid(format!("unknown field: {other}")));
            }
        }
    }

    if set.is_empty() {
        return Err(MoteError::Invalid(
            "set: at least one field=value is required".into(),
        ));
    }

    let op = make_patch(actor, id.clone(), expect, set, Timestamp::now());
    let name = publish::publish_op(&store, &op)?;

    let state2 = reducer::replay_store(&store)?;
    if state2.was_accepted(name.as_str()) {
        Ok(0)
    } else {
        let reason = state2
            .rejection_reason(name.as_str())
            .unwrap_or_else(|| "unknown".into());
        eprintln!("rejected: {reason}");
        Ok(2)
    }
}

fn clock_for(bead: &Bead, field: &str) -> MoteResult<String> {
    bead.clock
        .get(field)
        .cloned()
        .ok_or_else(|| MoteError::Invalid(format!("bead has no clock for `{field}`")))
}

fn cmd_show(store_flag: Option<&Path>, json_mode: bool, id: String) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let state = reducer::replay_store(&store)?;
    let bead = state
        .beads
        .get(&id)
        .ok_or_else(|| MoteError::Invalid(format!("no such bead {id}")))?;

    if json_mode {
        let v = serde_json::json!({
            "id": bead.id,
            "title": bead.title,
            "status": bead.status.as_str(),
            "priority": bead.priority,
            "body": bead.body,
            "assignee": bead.assignee,
            "tags": bead.tags.iter().collect::<Vec<_>>(),
            "deps": bead.deps.iter().map(|(p, k)| serde_json::json!({"parent": p, "kind": k})).collect::<Vec<_>>(),
            "notes": bead.notes.iter().map(|n| serde_json::json!({
                "op_id": n.op_id, "kind": n.note_kind, "actor": n.actor, "ts": n.ts, "text": n.text,
            })).collect::<Vec<_>>(),
            "ready": state.is_ready(bead),
            "deleted_at": bead.deleted_at_ts,
            "created_at": bead.created_at_ts,
            "clock": bead.clock,
        });
        println!("{}", serde_json::to_string(&v)?);
    } else {
        println!("id:       {}", bead.id);
        println!("title:    {}", bead.title);
        println!("status:   {}", bead.status.as_str());
        println!("priority: {}", bead.priority);
        if !bead.body.is_empty() {
            println!("body:     {}", bead.body);
        }
        if let Some(a) = &bead.assignee {
            println!("assignee: {a}");
        }
        if !bead.tags.is_empty() {
            println!(
                "tags:     {}",
                bead.tags.iter().cloned().collect::<Vec<_>>().join(", ")
            );
        }
        if !bead.deps.is_empty() {
            print!("deps:     ");
            let parts: Vec<String> = bead
                .deps
                .iter()
                .map(|(p, k)| format!("{p} ({k})"))
                .collect();
            println!("{}", parts.join(", "));
        }
        if !bead.notes.is_empty() {
            println!("notes:");
            for n in &bead.notes {
                println!("  [{}] {} {}: {}", n.note_kind, n.actor, n.ts, n.text);
            }
        }
        if state.is_ready(bead) {
            println!("ready:    yes");
        }
        if let Some(ts) = &bead.deleted_at_ts {
            println!("deleted_at: {ts}");
        }
    }
    Ok(0)
}

#[allow(clippy::too_many_arguments)]
fn cmd_ls(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    json_mode: bool,
    status: Option<String>,
    tag: Option<String>,
    assignee: Option<String>,
    all: bool,
    ready: bool,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let state = reducer::replay_store(&store)?;

    let want_status = match status.as_deref() {
        Some(s) => Some(
            Status::parse(s).ok_or_else(|| MoteError::Invalid(format!("invalid status: {s}")))?,
        ),
        None => None,
    };

    // For the --ready filter we need actor identity to exclude foreign claims.
    let (actor, now_ts) = if ready {
        let actor = store.resolve_actor(actor_flag).unwrap_or_default();
        let now = ids::format_rfc3339(Timestamp::now());
        (actor, now)
    } else {
        (String::new(), String::new())
    };

    let mut beads: Vec<&Bead> = state
        .live_beads()
        .filter(|b| {
            if !all && b.status == Status::Closed && want_status.is_none() {
                return false;
            }
            if let Some(s) = want_status {
                if b.status != s {
                    return false;
                }
            }
            if let Some(t) = &tag {
                if !b.tags.contains(t) {
                    return false;
                }
            }
            if let Some(a) = &assignee {
                if b.assignee.as_deref() != Some(a.as_str()) {
                    return false;
                }
            }
            if ready {
                if !state.is_ready(b) {
                    return false;
                }
                if let Some(c) = &b.claim {
                    if c.is_live(&now_ts) && c.claimed_by != actor {
                        return false;
                    }
                }
            }
            true
        })
        .collect();

    beads.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.id.cmp(&b.id)));

    if json_mode {
        let arr: Vec<_> = beads
            .iter()
            .map(|b| {
                serde_json::json!({
                    "id": b.id,
                    "title": b.title,
                    "status": b.status.as_str(),
                    "priority": b.priority,
                    "tags": b.tags.iter().collect::<Vec<_>>(),
                    "assignee": b.assignee,
                })
            })
            .collect();
        println!("{}", serde_json::to_string(&arr)?);
    } else {
        for b in &beads {
            println!(
                "{:<24} p{} {:<8} {}",
                b.id,
                b.priority,
                b.status.as_str(),
                b.title
            );
        }
    }
    Ok(0)
}

fn cmd_history(
    store_flag: Option<&Path>,
    json_mode: bool,
    id: String,
    include_rejected: bool,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let state = reducer::replay_store(&store)?;
    let entries = state
        .history
        .get(&id)
        .ok_or_else(|| MoteError::Invalid(format!("no such bead {id}")))?;

    let filtered: Vec<_> = entries
        .iter()
        .filter(|e| include_rejected || e.accepted)
        .collect();

    if json_mode {
        let arr: Vec<_> = filtered
            .iter()
            .map(|e| {
                serde_json::json!({
                    "op_id": e.op_id,
                    "kind": e.kind,
                    "actor": e.actor,
                    "ts": e.ts,
                    "accepted": e.accepted,
                    "reason": e.reason,
                })
            })
            .collect();
        println!("{}", serde_json::to_string(&arr)?);
    } else {
        for e in &filtered {
            let tag = if e.accepted { "ACCEPT" } else { "REJECT" };
            let reason = e.reason.as_deref().unwrap_or("");
            println!(
                "{} {} {} {} {} {}",
                tag, e.op_id, e.kind, e.actor, e.ts, reason
            );
        }
    }
    Ok(0)
}

fn cmd_ready(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    json_mode: bool,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let state = reducer::replay_store(&store)?;
    // If actor is unresolved, fall back to "" so any non-empty `claimed_by`
    // (i.e. all live foreign claims) is excluded — the safe default.
    let actor = store.resolve_actor(actor_flag).unwrap_or_default();
    let now = ids::format_rfc3339(Timestamp::now());
    let mut beads: Vec<&Bead> = state.ready_beads_for(&actor, &now).collect();
    beads.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.id.cmp(&b.id)));

    if json_mode {
        let arr: Vec<_> = beads
            .iter()
            .map(|b| {
                serde_json::json!({
                    "id": b.id,
                    "title": b.title,
                    "priority": b.priority,
                    "tags": b.tags.iter().collect::<Vec<_>>(),
                    "assignee": b.assignee,
                })
            })
            .collect();
        println!("{}", serde_json::to_string(&arr)?);
    } else {
        for b in &beads {
            println!(
                "{:<24} p{} {:<8} {}",
                b.id,
                b.priority,
                b.status.as_str(),
                b.title
            );
        }
    }
    Ok(0)
}

fn cmd_note(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    id: String,
    note_kind: String,
    text: String,
) -> MoteResult<i32> {
    if !validate_note_kind(&note_kind) {
        return Err(MoteError::Invalid(format!(
            "invalid note_kind `{note_kind}` (expected one of: {})",
            op::VALID_NOTE_KINDS.join(" | ")
        )));
    }
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;
    let op = make_note(actor, id, note_kind, text, Timestamp::now());
    let name = publish::publish_op(&store, &op)?;
    verify_accept(&store, &name)
}

fn cmd_dep(actor_flag: Option<&str>, store_flag: Option<&Path>, cmd: DepCmd) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;
    let (add, child, parent, kind) = match cmd {
        DepCmd::Add {
            child,
            parent,
            kind,
        } => (true, child, parent, kind),
        DepCmd::Rm {
            child,
            parent,
            kind,
        } => (false, child, parent, kind),
    };
    let op = make_dep(add, actor, child, parent, kind, Timestamp::now());
    let name = publish::publish_op(&store, &op)?;
    verify_accept(&store, &name)
}

fn cmd_tag(actor_flag: Option<&str>, store_flag: Option<&Path>, cmd: TagCmd) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;
    let (add, id, tag) = match cmd {
        TagCmd::Add { id, tag } => (true, id, tag),
        TagCmd::Rm { id, tag } => (false, id, tag),
    };
    let op = make_tag(add, actor, id, tag, Timestamp::now());
    let name = publish::publish_op(&store, &op)?;
    verify_accept(&store, &name)
}

fn cmd_close(actor_flag: Option<&str>, store_flag: Option<&Path>, id: String) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;
    let state = reducer::replay_store(&store)?;
    let bead = state
        .beads
        .get(&id)
        .ok_or_else(|| MoteError::Invalid(format!("no such bead {id}")))?;
    if bead.is_deleted() {
        return Err(MoteError::Invalid(format!("bead {id} is deleted")));
    }
    let mut expect = BTreeMap::new();
    if let Some(c) = bead.clock.get("status") {
        expect.insert("status".to_string(), c.clone());
    }
    let op = make_close(actor, id, expect, Timestamp::now());
    let name = publish::publish_op(&store, &op)?;
    verify_accept(&store, &name)
}

fn cmd_delete(actor_flag: Option<&str>, store_flag: Option<&Path>, id: String) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;
    let op = make_delete(actor, id, Timestamp::now());
    let name = publish::publish_op(&store, &op)?;
    verify_accept(&store, &name)
}

fn cmd_claim(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    id: String,
    ttl: Option<u32>,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;
    let format = store.read_format()?;
    let ttl_s = ttl.unwrap_or(format.default_ttl_s.claim);

    // If a same-actor claim already exists, auto-fill expect_claim so renewal
    // succeeds against the strict reducer rule.
    let state = reducer::replay_store(&store)?;
    let expect_claim = state
        .beads
        .get(&id)
        .and_then(|b| b.claim.as_ref())
        .filter(|c| c.claimed_by == actor)
        .map(|c| c.claim_clock.clone());

    let op = make_claim(
        actor.clone(),
        id,
        actor,
        ttl_s,
        expect_claim,
        Timestamp::now(),
    );
    let name = publish::publish_op(&store, &op)?;
    verify_accept(&store, &name)
}

fn cmd_release(actor_flag: Option<&str>, store_flag: Option<&Path>, id: String) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;
    let op = make_release(actor, id, None, Timestamp::now());
    let name = publish::publish_op(&store, &op)?;
    verify_accept(&store, &name)
}

fn cmd_msg(actor_flag: Option<&str>, store_flag: Option<&Path>, cmd: MsgCmd) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;
    match cmd {
        MsgCmd::Send {
            to,
            issue,
            reservation,
            msg_kind,
            text,
        } => {
            if !validate_msg_kind(&msg_kind) {
                return Err(MoteError::Invalid(format!(
                    "invalid msg_kind `{msg_kind}` (expected one of: {})",
                    op::VALID_MSG_KINDS.join(" | ")
                )));
            }
            let msg_id = ids::new_msg_id();
            let op = make_msg_send(
                actor,
                msg_id.clone(),
                to,
                issue,
                reservation,
                msg_kind,
                text,
                Timestamp::now(),
            );
            let name = publish::publish_op(&store, &op)?;
            // Print msg_id on stdout so callers can ack later.
            println!("{msg_id}");
            verify_accept(&store, &name)
        }
        MsgCmd::Ack { msg_id } => {
            let op = make_msg_ack(actor, msg_id, Timestamp::now());
            let name = publish::publish_op(&store, &op)?;
            verify_accept(&store, &name)
        }
    }
}

fn cmd_discuss(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    json_mode: bool,
    cmd: DiscussCmd,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;

    match cmd {
        DiscussCmd::Post {
            topic,
            reply_to,
            text,
        } => {
            let actor = store.resolve_actor(actor_flag)?;
            let topic = normalize_discussion_topic(&topic)?;
            if text.trim().is_empty() {
                return Err(MoteError::Invalid(
                    "discussion post text must be non-empty".into(),
                ));
            }
            let post_id = ids::new_post_id();
            let op = make_board_post(
                actor,
                post_id.clone(),
                topic,
                text,
                reply_to,
                Timestamp::now(),
            );
            let name = publish::publish_op(&store, &op)?;
            println!("{post_id}");
            verify_accept(&store, &name)
        }
        DiscussCmd::List { topic, limit } => {
            let state = reducer::replay_store(&store)?;
            let normalized_topic = topic
                .as_deref()
                .map(normalize_discussion_topic)
                .transpose()?;
            let mut posts = state.board_posts_for(normalized_topic.as_deref());
            if let Some(limit) = limit {
                if posts.len() > limit {
                    posts = posts.split_off(posts.len() - limit);
                }
            }
            print_board_posts(posts, json_mode)
        }
        DiscussCmd::Unread { topic, limit } => {
            let actor = store.resolve_actor(actor_flag)?;
            let state = reducer::replay_store(&store)?;
            let normalized_topic = topic
                .as_deref()
                .map(normalize_discussion_topic)
                .transpose()?;
            let mut posts = state.unread_board_posts_for(&actor, normalized_topic.as_deref());
            if let Some(limit) = limit {
                if posts.len() > limit {
                    posts = posts.split_off(posts.len() - limit);
                }
            }
            print_board_posts(posts, json_mode)
        }
        DiscussCmd::MarkRead { topic } => {
            let actor = store.resolve_actor(actor_flag)?;
            let state = reducer::replay_store(&store)?;
            let normalized_topic = topic
                .as_deref()
                .map(normalize_discussion_topic)
                .transpose()?;
            let latest = state
                .board_posts_for(normalized_topic.as_deref())
                .into_iter()
                .max_by(|a, b| a.sent_op_id.cmp(&b.sent_op_id));
            let Some(latest) = latest else {
                return Ok(0);
            };
            let op = make_board_read(
                actor,
                latest.sent_op_id.clone(),
                normalized_topic,
                Timestamp::now(),
            );
            let name = publish::publish_op(&store, &op)?;
            println!("{}", latest.post_id);
            verify_accept(&store, &name)
        }
        DiscussCmd::Replies { post_id } => {
            let state = reducer::replay_store(&store)?;
            if !state.board_posts.contains_key(&post_id) {
                return Err(MoteError::Invalid(format!("no such post {post_id}")));
            }
            print_board_posts(state.replies_to(&post_id), json_mode)
        }
        DiscussCmd::Thread { post_id } => {
            let state = reducer::replay_store(&store)?;
            if !state.board_posts.contains_key(&post_id) {
                return Err(MoteError::Invalid(format!("no such post {post_id}")));
            }
            print_thread_posts(state.thread_posts(&post_id), json_mode)
        }
        DiscussCmd::Topic { cmd } => match cmd {
            DiscussTopicCmd::New { topic, title, body } => {
                let actor = store.resolve_actor(actor_flag)?;
                let topic = normalize_discussion_topic(&topic)?;
                let op = make_board_topic(actor, topic.clone(), title, body, Timestamp::now());
                let name = publish::publish_op(&store, &op)?;
                println!("{topic}");
                verify_accept(&store, &name)
            }
        },
        DiscussCmd::Search {
            query,
            topic,
            limit,
        } => {
            let state = reducer::replay_store(&store)?;
            let query = query.trim();
            if query.is_empty() {
                return Err(MoteError::Invalid(
                    "discussion search query must be non-empty".into(),
                ));
            }
            let normalized_topic = topic
                .as_deref()
                .map(normalize_discussion_topic)
                .transpose()?;
            print_discussion_search(&state, query, normalized_topic.as_deref(), limit, json_mode)
        }
        DiscussCmd::Sticky { post_id } => {
            let actor = store.resolve_actor(actor_flag)?;
            let op = make_board_sticky(actor, post_id, true, Timestamp::now());
            let name = publish::publish_op(&store, &op)?;
            verify_accept(&store, &name)
        }
        DiscussCmd::Unsticky { post_id } => {
            let actor = store.resolve_actor(actor_flag)?;
            let op = make_board_sticky(actor, post_id, false, Timestamp::now());
            let name = publish::publish_op(&store, &op)?;
            verify_accept(&store, &name)
        }
        DiscussCmd::Topics => {
            let state = reducer::replay_store(&store)?;
            print_discussion_topics(state.board_topics_by_activity(), json_mode)
        }
    }
}

fn print_discussion_topics(
    topics: Vec<&crate::state::BoardTopicRecord>,
    json_mode: bool,
) -> MoteResult<i32> {
    if json_mode {
        let arr: Vec<_> = topics.iter().map(|t| topic_json(t)).collect();
        println!("{}", serde_json::to_string(&arr)?);
    } else {
        for t in &topics {
            let explicit = if t.explicit { "explicit" } else { "implicit" };
            println!(
                "{}  posts={}  sticky={}  created={}  last={}  {}  {}",
                t.topic,
                t.post_count,
                t.sticky_count,
                t.created_ts,
                t.last_activity_ts,
                explicit,
                t.title
            );
        }
    }
    Ok(0)
}

fn topic_json(t: &crate::state::BoardTopicRecord) -> serde_json::Value {
    serde_json::json!({
        "topic": t.topic,
        "title": t.title,
        "body": t.body,
        "created_by": t.created_by,
        "created_ts": t.created_ts,
        "created_op_id": t.created_op_id,
        "explicit": t.explicit,
        "last_activity_ts": t.last_activity_ts,
        "last_activity_op_id": t.last_activity_op_id,
        "post_count": t.post_count,
        "sticky_count": t.sticky_count,
    })
}

fn print_discussion_search(
    state: &crate::state::State,
    query: &str,
    topic_filter: Option<&str>,
    limit: Option<usize>,
    json_mode: bool,
) -> MoteResult<i32> {
    let needle = query.to_ascii_lowercase();
    let matches_query = |s: &str| s.to_ascii_lowercase().contains(&needle);

    let mut topics: Vec<&crate::state::BoardTopicRecord> = state
        .board_topics_by_activity()
        .into_iter()
        .filter(|t| {
            topic_filter.is_none_or(|wanted| t.topic == wanted)
                && (matches_query(&t.topic) || matches_query(&t.title) || matches_query(&t.body))
        })
        .collect();
    let mut posts: Vec<&crate::state::BoardPostRecord> = state
        .board_posts_for(topic_filter)
        .into_iter()
        .filter(|p| {
            matches_query(&p.post_id)
                || matches_query(&p.topic)
                || matches_query(&p.from)
                || matches_query(&p.body)
        })
        .collect();

    if let Some(limit) = limit {
        topics.truncate(limit);
        posts.truncate(limit);
    }

    if json_mode {
        let v = serde_json::json!({
            "topics": topics.iter().map(|t| topic_json(t)).collect::<Vec<_>>(),
            "posts": posts.iter().map(|p| board_post_json(p)).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string(&v)?);
    } else {
        for t in &topics {
            println!(
                "topic  {}  posts={}  sticky={}  created={}  last={}  {}",
                t.topic, t.post_count, t.sticky_count, t.created_ts, t.last_activity_ts, t.title
            );
        }
        for p in &posts {
            let sticky = if p.sticky { " sticky" } else { "" };
            let reply = p.reply_to.as_deref().unwrap_or("-");
            println!(
                "post   {}{}  {}  from={}  topic={}  reply={}  {}",
                p.post_id, sticky, p.sent_ts, p.from, p.topic, reply, p.body
            );
        }
    }
    Ok(0)
}

fn print_board_posts(
    posts: Vec<&crate::state::BoardPostRecord>,
    json_mode: bool,
) -> MoteResult<i32> {
    if json_mode {
        let arr: Vec<_> = posts.iter().map(|p| board_post_json(p)).collect();
        println!("{}", serde_json::to_string(&arr)?);
    } else {
        for p in &posts {
            let reply = p.reply_to.as_deref().unwrap_or("-");
            let sticky = if p.sticky { " sticky" } else { "" };
            println!(
                "{}{}  {}  from={}  topic={}  reply={}  {}",
                p.post_id, sticky, p.sent_ts, p.from, p.topic, reply, p.body
            );
        }
    }
    Ok(0)
}

fn print_thread_posts(
    posts: Vec<(usize, &crate::state::BoardPostRecord)>,
    json_mode: bool,
) -> MoteResult<i32> {
    if json_mode {
        let arr: Vec<_> = posts
            .iter()
            .map(|(depth, post)| {
                let mut v = board_post_json(post);
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("depth".into(), serde_json::json!(depth));
                }
                v
            })
            .collect();
        println!("{}", serde_json::to_string(&arr)?);
    } else {
        for (depth, post) in &posts {
            let indent = "  ".repeat(*depth);
            let reply = post.reply_to.as_deref().unwrap_or("-");
            let sticky = if post.sticky { " sticky" } else { "" };
            println!(
                "{}{}{}  {}  from={}  topic={}  reply={}  {}",
                indent, post.post_id, sticky, post.sent_ts, post.from, post.topic, reply, post.body
            );
        }
    }
    Ok(0)
}

fn board_post_json(p: &crate::state::BoardPostRecord) -> serde_json::Value {
    serde_json::json!({
        "post_id": p.post_id,
        "from": p.from,
        "topic": p.topic,
        "body": p.body,
        "reply_to": p.reply_to,
        "sticky": p.sticky,
        "sticky_op_id": p.sticky_op_id,
        "sent_ts": p.sent_ts,
    })
}

fn normalize_discussion_topic(topic: &str) -> MoteResult<String> {
    let topic = topic.trim();
    if topic.is_empty() {
        return Err(MoteError::Invalid(
            "discussion topic must be non-empty".into(),
        ));
    }
    if topic.chars().any(|c| c == '\0' || c == '\n' || c == '\r') {
        return Err(MoteError::Invalid(
            "discussion topic must be a single-line string".into(),
        ));
    }
    Ok(topic.to_string())
}

fn cmd_inbox(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    json_mode: bool,
    issue: Option<String>,
    from: Option<String>,
    kind: Option<String>,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;
    let state = reducer::replay_store(&store)?;
    let messages = state.inbox_for(&actor);
    let filtered: Vec<&crate::state::MsgRecord> = messages
        .into_iter()
        .filter(|m| {
            issue
                .as_deref()
                .is_none_or(|i| m.entity.as_deref() == Some(i))
                && from.as_deref().is_none_or(|f| m.from == f)
                && kind.as_deref().is_none_or(|k| m.msg_kind == k)
        })
        .collect();

    if json_mode {
        let arr: Vec<_> = filtered
            .iter()
            .map(|m| {
                serde_json::json!({
                    "msg_id": m.msg_id,
                    "from": m.from,
                    "to": m.to,
                    "entity": m.entity,
                    "reservation": m.reservation,
                    "msg_kind": m.msg_kind,
                    "body": m.body,
                    "sent_ts": m.sent_ts,
                })
            })
            .collect();
        println!("{}", serde_json::to_string(&arr)?);
    } else {
        for m in &filtered {
            let issue_s = m.entity.as_deref().unwrap_or("-");
            println!(
                "{}  {}  from={}  issue={}  kind={}  {}",
                m.msg_id, m.sent_ts, m.from, issue_s, m.msg_kind, m.body
            );
        }
    }
    Ok(0)
}

fn cmd_reserve(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    paths: Vec<String>,
    issue: String,
    ttl: Option<u32>,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;
    let format = store.read_format()?;
    let ttl_s = ttl.unwrap_or(format.default_ttl_s.reservation);
    if paths.is_empty() {
        return Err(MoteError::Invalid("at least one path required".into()));
    }
    let rv_id = ids::new_reservation_id();
    let op = make_reserve_open(actor, rv_id.clone(), issue, paths, ttl_s, Timestamp::now());
    let name = publish::publish_op(&store, &op)?;
    let state = reducer::replay_store(&store)?;
    if state.was_accepted(name.as_str()) {
        println!("{rv_id}");
        Ok(0)
    } else {
        let reason = state
            .rejection_reason(name.as_str())
            .unwrap_or_else(|| "unknown".into());
        eprintln!("reserve rejected: {reason}");
        Ok(2)
    }
}

fn cmd_unreserve(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    rv: String,
    paths: Vec<String>,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;
    let p = if paths.is_empty() { None } else { Some(paths) };
    let op = make_reserve_close(actor, rv, p, Timestamp::now());
    let name = publish::publish_op(&store, &op)?;
    verify_accept(&store, &name)
}

fn cmd_preflight(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    json_mode: bool,
    issue: String,
    paths: Vec<String>,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;
    let state = reducer::replay_store(&store)?;
    let now = ids::format_rfc3339(Timestamp::now());

    let mut normalized = Vec::with_capacity(paths.len());
    for p in &paths {
        let n = crate::paths::normalize(p)
            .map_err(|e| MoteError::Invalid(format!("path `{p}`: {e}")))?;
        normalized.push(n);
    }

    let mut conflicts: Vec<(String, String, String, String)> = Vec::new();
    // (new_path, held_path, holder_actor, reservation_id)
    for r in state.reservations.values() {
        if r.actor == actor || !r.is_live(&now) {
            continue;
        }
        for p_new in &normalized {
            for p_held in r
                .paths
                .iter()
                .filter(|p| !r.closed_paths.contains(p.as_str()))
            {
                if crate::paths::overlap(p_new, p_held) {
                    conflicts.push((
                        p_new.clone(),
                        p_held.clone(),
                        r.actor.clone(),
                        r.reservation_id.clone(),
                    ));
                }
            }
        }
    }

    let issue_status = state
        .beads
        .get(&issue)
        .map(|b| b.status.as_str().to_string());
    let claim_holder = state
        .beads
        .get(&issue)
        .and_then(|b| b.claim.as_ref().map(|c| c.claimed_by.clone()));

    if json_mode {
        let v = serde_json::json!({
            "issue": issue,
            "issue_status": issue_status,
            "claim_holder": claim_holder,
            "actor": actor,
            "paths": normalized,
            "conflicts": conflicts.iter().map(|(p_new, p_held, who, rv)| serde_json::json!({
                "new_path": p_new, "held_path": p_held, "actor": who, "reservation_id": rv,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string(&v)?);
    } else {
        println!(
            "issue:    {issue} ({})",
            issue_status.as_deref().unwrap_or("unknown")
        );
        if let Some(h) = &claim_holder {
            println!("claim:    held by {h}");
        }
        if conflicts.is_empty() {
            println!("paths:    {} clear", normalized.len());
        } else {
            println!("conflicts:");
            for (p_new, p_held, who, rv) in &conflicts {
                println!("  {p_new} overlaps {p_held} held by {who} (rv {rv})");
            }
        }
    }

    Ok(if conflicts.is_empty() { 0 } else { 2 })
}

fn cmd_begin(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    id: String,
    paths: Vec<String>,
    note: Option<String>,
    ttl: Option<u32>,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;
    let format = store.read_format()?;
    let reserve_ttl = ttl.unwrap_or(format.default_ttl_s.reservation);
    let claim_ttl = format.default_ttl_s.claim;

    if paths.is_empty() {
        return Err(MoteError::Invalid("at least one path required".into()));
    }

    // Step 1: reserve_open
    let rv_id = ids::new_reservation_id();
    let reserve = make_reserve_open(
        actor.clone(),
        rv_id.clone(),
        id.clone(),
        paths,
        reserve_ttl,
        Timestamp::now(),
    );
    let reserve_name = publish::publish_op(&store, &reserve)?;
    let state1 = reducer::replay_store(&store)?;
    if !state1.was_accepted(reserve_name.as_str()) {
        let reason = state1
            .rejection_reason(reserve_name.as_str())
            .unwrap_or_else(|| "unknown".into());
        eprintln!("reserve_open rejected: {reason}");
        return Ok(2);
    }

    // Step 2: claim
    let expect_claim = state1
        .beads
        .get(&id)
        .and_then(|b| b.claim.as_ref())
        .filter(|c| c.claimed_by == actor)
        .map(|c| c.claim_clock.clone());
    let claim_op = make_claim(
        actor.clone(),
        id.clone(),
        actor.clone(),
        claim_ttl,
        expect_claim,
        Timestamp::now(),
    );
    let claim_name = publish::publish_op(&store, &claim_op)?;
    let state2 = reducer::replay_store(&store)?;
    if !state2.was_accepted(claim_name.as_str()) {
        let reason = state2
            .rejection_reason(claim_name.as_str())
            .unwrap_or_else(|| "unknown".into());
        eprintln!("claim rejected: {reason}");
        // Compensating reserve_close.
        let close = make_reserve_close(actor.clone(), rv_id, None, Timestamp::now());
        let _ = publish::publish_op(&store, &close);
        return Ok(2);
    }

    // Step 3: optional progress note (best effort).
    if let Some(text) = note {
        let note_op = make_note(actor, id, "progress".into(), text, Timestamp::now());
        let _ = publish::publish_op(&store, &note_op);
    }

    println!("{rv_id}");
    Ok(0)
}

fn cmd_handoff(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    id: String,
    to: String,
    note: Option<String>,
    release: bool,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;
    let format = store.read_format()?;

    // Note (handoff)
    let text = note.unwrap_or_else(|| format!("handing off to {to}"));
    let note_op = make_note(
        actor.clone(),
        id.clone(),
        "handoff".into(),
        text,
        Timestamp::now(),
    );
    let _ = publish::publish_op(&store, &note_op);

    // Claim reassignment (auto-fill expect_claim against current claim_clock if any)
    let state = reducer::replay_store(&store)?;
    let expect_claim = state
        .beads
        .get(&id)
        .and_then(|b| b.claim.as_ref())
        .map(|c| c.claim_clock.clone());
    let claim = make_claim(
        actor.clone(),
        id.clone(),
        to,
        format.default_ttl_s.claim,
        expect_claim,
        Timestamp::now(),
    );
    let name = publish::publish_op(&store, &claim)?;
    let state2 = reducer::replay_store(&store)?;
    if !state2.was_accepted(name.as_str()) {
        let reason = state2
            .rejection_reason(name.as_str())
            .unwrap_or_else(|| "unknown".into());
        eprintln!("handoff claim rejected: {reason}");
        return Ok(2);
    }

    if release {
        let state3 = reducer::replay_store(&store)?;
        let now = ids::format_rfc3339(Timestamp::now());
        let mine: Vec<String> = state3
            .reservations
            .values()
            .filter(|r| r.actor == actor && r.entity == id && r.is_active(&now))
            .map(|r| r.reservation_id.clone())
            .collect();
        for rv in mine {
            let close = make_reserve_close(actor.clone(), rv, None, Timestamp::now());
            let _ = publish::publish_op(&store, &close);
        }
    }
    Ok(0)
}

fn cmd_done(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    id: String,
    note: Option<String>,
) -> MoteResult<i32> {
    use std::collections::BTreeMap;
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;

    // Completion note (note_kind=note). Best effort.
    let text = note.unwrap_or_else(|| "done".into());
    let note_op = make_note(
        actor.clone(),
        id.clone(),
        "note".into(),
        text,
        Timestamp::now(),
    );
    let note_name = publish::publish_op(&store, &note_op)?;

    // Close (with expect.status). MUST be accepted for `done` to mean closed.
    let state = reducer::replay_store(&store)?;
    let bead = state
        .beads
        .get(&id)
        .ok_or_else(|| MoteError::Invalid(format!("no such bead {id}")))?;
    let mut expect = BTreeMap::new();
    if let Some(c) = bead.clock.get("status") {
        expect.insert("status".to_string(), c.clone());
    }
    // Test-only hook: widen the read-modify-write window so a competing
    // `mote set ... status=...` from another actor can land between this
    // observation and the close publish, deterministically reproducing a
    // stale-clock race. No effect when the env var is unset.
    maybe_test_sleep("MOTE_TEST_DELAY_BEFORE_CLOSE_MS");
    let close = make_close(actor.clone(), id.clone(), expect, Timestamp::now());
    let close_name = publish::publish_op(&store, &close)?;
    let state_post_close = reducer::replay_store(&store)?;
    if !state_post_close.was_accepted(close_name.as_str()) {
        let reason = state_post_close
            .rejection_reason(close_name.as_str())
            .unwrap_or_else(|| "unknown".into());
        eprintln!("close rejected: {reason}");
        return Ok(2);
    }

    // Close any reservations this actor holds on this issue. Each is required
    // (warn but do not fail the command if a reserve_close is rejected — we
    // already closed the bead and that is the visible user intent).
    let state2 = reducer::replay_store(&store)?;
    let now = ids::format_rfc3339(Timestamp::now());
    let mine: Vec<String> = state2
        .reservations
        .values()
        .filter(|r| r.actor == actor && r.entity == id && r.is_active(&now))
        .map(|r| r.reservation_id.clone())
        .collect();
    for rv in mine {
        let cls = make_reserve_close(actor.clone(), rv.clone(), None, Timestamp::now());
        let cls_name = publish::publish_op(&store, &cls)?;
        let state_n = reducer::replay_store(&store)?;
        if !state_n.was_accepted(cls_name.as_str()) {
            let reason = state_n
                .rejection_reason(cls_name.as_str())
                .unwrap_or_else(|| "unknown".into());
            eprintln!("warning: reserve_close on {rv} rejected: {reason}");
        }
    }

    // Release the claim if held by this actor.
    let state3 = reducer::replay_store(&store)?;
    if let Some(c) = state3.beads.get(&id).and_then(|b| b.claim.as_ref()) {
        if c.claimed_by == actor {
            let rel = make_release(actor, id, None, Timestamp::now());
            let rel_name = publish::publish_op(&store, &rel)?;
            let state_r = reducer::replay_store(&store)?;
            if !state_r.was_accepted(rel_name.as_str()) {
                let reason = state_r
                    .rejection_reason(rel_name.as_str())
                    .unwrap_or_else(|| "unknown".into());
                eprintln!("warning: release rejected: {reason}");
            }
        }
    }

    let _ = note_name; // kept for symmetry; intentionally unused
    Ok(0)
}

fn cmd_who_has(store_flag: Option<&Path>, json_mode: bool, path: String) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let state = reducer::replay_store(&store)?;
    let now = ids::format_rfc3339(Timestamp::now());
    let normalized = crate::paths::normalize(&path)
        .map_err(|e| MoteError::Invalid(format!("path `{path}`: {e}")))?;

    let mut hits: Vec<(String, String, String, String, String)> = Vec::new();
    // (held_path, actor, reservation_id, entity, lease_until_ts)
    for r in state.reservations.values() {
        if !r.is_live(&now) {
            continue;
        }
        for p_held in r
            .paths
            .iter()
            .filter(|p| !r.closed_paths.contains(p.as_str()))
        {
            if crate::paths::overlap(&normalized, p_held) {
                hits.push((
                    p_held.clone(),
                    r.actor.clone(),
                    r.reservation_id.clone(),
                    r.entity.clone(),
                    r.lease_until_ts.clone(),
                ));
            }
        }
    }

    if json_mode {
        let arr: Vec<_> = hits
            .iter()
            .map(|(p, a, rv, e, until)| {
                serde_json::json!({
                    "path": p, "actor": a, "reservation_id": rv,
                    "entity": e, "lease_until_ts": until,
                })
            })
            .collect();
        println!("{}", serde_json::to_string(&arr)?);
    } else if hits.is_empty() {
        println!("no live reservations overlap {normalized}");
    } else {
        for (p, a, rv, e, until) in &hits {
            println!("  {p} held by {a} (issue {e}, rv {rv}, until {until})");
        }
    }
    Ok(0)
}

fn cmd_board(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    json_mode: bool,
) -> MoteResult<i32> {
    use std::collections::BTreeMap;
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag).ok();
    let state = reducer::replay_store(&store)?;
    let now = ids::format_rfc3339(Timestamp::now());

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for b in state.live_beads() {
        *counts.entry(b.status.as_str().to_string()).or_insert(0) += 1;
    }
    let active_claims: Vec<&Bead> = state
        .live_beads()
        .filter(|b| b.claim.as_ref().is_some_and(|c| c.is_live(&now)))
        .collect();
    let active_reservations: Vec<_> = state
        .reservations
        .values()
        .filter(|r| r.is_active(&now))
        .collect();
    let inbox_count = actor
        .as_ref()
        .map(|a| state.inbox_for(a).len())
        .unwrap_or(0);
    let discussion_unread_count = actor
        .as_ref()
        .map(|a| state.unread_board_posts_for(a, None).len())
        .unwrap_or(0);

    if json_mode {
        let v = serde_json::json!({
            "actor": actor,
            "status_counts": counts,
            "active_claims": active_claims.iter().map(|b| serde_json::json!({
                "id": b.id, "title": b.title, "status": b.status.as_str(),
                "claimed_by": b.claim.as_ref().map(|c| &c.claimed_by),
                "lease_until_ts": b.claim.as_ref().map(|c| &c.lease_until_ts),
            })).collect::<Vec<_>>(),
            "active_reservations": active_reservations.iter().map(|r| serde_json::json!({
                "reservation_id": r.reservation_id, "actor": r.actor, "entity": r.entity,
                "paths": r.live_paths(), "lease_until_ts": r.lease_until_ts,
            })).collect::<Vec<_>>(),
            "inbox_unacked": inbox_count,
            "discussion_unread": discussion_unread_count,
        });
        println!("{}", serde_json::to_string(&v)?);
    } else {
        if let Some(a) = &actor {
            println!("actor:        {a}");
        }
        println!("status:");
        for (k, v) in &counts {
            println!("  {k:<8}  {v}");
        }
        println!("claims:       {} active", active_claims.len());
        for b in &active_claims {
            let holder = b
                .claim
                .as_ref()
                .map(|c| c.claimed_by.as_str())
                .unwrap_or("?");
            println!("  {} ({}, by {holder})", b.id, b.status.as_str());
        }
        println!("reservations: {} active", active_reservations.len());
        for r in &active_reservations {
            let live = r.live_paths().join(", ");
            println!(
                "  {} by {} on {}: {}",
                r.reservation_id, r.actor, r.entity, live
            );
        }
        println!("inbox:        {inbox_count} unacked");
        println!("discussion:   {discussion_unread_count} unread");
    }
    Ok(0)
}

fn verify_accept(store: &Store, name: &ids::OpName) -> MoteResult<i32> {
    let state = reducer::replay_store(store)?;
    if state.was_accepted(name.as_str()) {
        Ok(0)
    } else {
        let reason = state
            .rejection_reason(name.as_str())
            .unwrap_or_else(|| "unknown".into());
        eprintln!("rejected: {reason}");
        Ok(2)
    }
}

fn cmd_doctor(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    json_mode: bool,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;

    let root_ok = store.root().is_dir();
    let ops_ok = store.ops_dir().is_dir();
    let tmp_ok = store.tmp_dir().is_dir();
    let local_ok = store.local_dir().is_dir();
    let layout_ok = root_ok && ops_ok && tmp_ok && local_ok;

    let (schema_version, format_ok, format_error) = match store.read_format() {
        Ok(format) => (
            Some(format.schema_version),
            format.schema_version == 1,
            None::<String>,
        ),
        Err(e) => (None, false, Some(e.to_string())),
    };

    let actor = match resolve_actor_with_source(&store, actor_flag) {
        Ok(actor) => Some(actor),
        Err(MoteError::ActorUnresolved) => None,
        Err(e) => return Err(e),
    };

    let (fsck_report, fsck_error) = if ops_ok && tmp_ok {
        match fsck::run(&store, false) {
            Ok(report) => (Some(report), None::<String>),
            Err(e) => (None, Some(e.to_string())),
        }
    } else {
        (
            None,
            Some("ops/ or tmp/ directory is missing; fsck not run".into()),
        )
    };
    let fsck_clean = fsck_report.as_ref().is_some_and(|r| r.is_clean());
    let storage_ok = layout_ok && format_ok && fsck_error.is_none() && fsck_clean;
    let actor_ok = actor.is_some();
    let ok = storage_ok && actor_ok;

    if json_mode {
        let v = serde_json::json!({
            "ok": ok,
            "store_root": store.root().display().to_string(),
            "layout": {
                "root": root_ok,
                "ops": ops_ok,
                "tmp": tmp_ok,
                "local": local_ok,
            },
            "format": {
                "ok": format_ok,
                "schema_version": schema_version,
                "error": format_error,
            },
            "actor_ok": actor_ok,
            "actor": actor.as_ref().map(|a| a.actor.as_str()),
            "actor_source": actor.as_ref().map(|a| a.source),
            "fsck_clean": fsck_clean,
            "fsck_error": fsck_error,
            "fsck": fsck_report.as_ref().map(fsck_report_json),
        });
        println!("{}", serde_json::to_string(&v)?);
    } else {
        println!("store:  {}", store.root().display());
        match (format_ok, schema_version, format_error.as_deref()) {
            (true, Some(version), _) => println!("format: ok (schema_version {version})"),
            (_, Some(version), _) => println!("format: bad schema_version {version}"),
            (_, _, Some(error)) => println!("format: bad ({error})"),
            _ => println!("format: bad"),
        }
        println!("layout: {}", if layout_ok { "ok" } else { "bad" });
        if !layout_ok {
            println!("  root:  {root_ok}");
            println!("  ops:   {ops_ok}");
            println!("  tmp:   {tmp_ok}");
            println!("  local: {local_ok}");
        }
        match &actor {
            Some(actor) => println!("actor:  {} ({})", actor.actor, actor.source),
            None => println!("actor:  unresolved"),
        }
        match (&fsck_report, &fsck_error) {
            (Some(report), _) if report.is_clean() => {
                println!(
                    "fsck:   clean ({} ops, {} tmp entries)",
                    report.ops_checked, report.tmp_total
                );
            }
            (Some(report), _) => {
                println!(
                    "fsck:   bad ({} ops, {} bad filenames, {} bad json, {} bad hashes, {} bad shapes)",
                    report.ops_checked,
                    report.bad_filename.len(),
                    report.bad_json.len(),
                    report.bad_hash.len(),
                    report.bad_op_shape.len(),
                );
            }
            (_, Some(error)) => println!("fsck:   not run ({error})"),
            _ => println!("fsck:   not run"),
        }
    }

    Ok(if !storage_ok {
        4
    } else if !actor_ok {
        3
    } else {
        0
    })
}

fn fsck_report_json(report: &fsck::FsckReport) -> serde_json::Value {
    let bad_hash_json: Vec<_> = report
        .bad_hash
        .iter()
        .map(|(f, w, g)| serde_json::json!({"file": f, "expected": w, "got": g}))
        .collect();
    let bad_shape_json: Vec<_> = report
        .bad_op_shape
        .iter()
        .map(|(f, e)| serde_json::json!({"file": f, "error": e}))
        .collect();

    serde_json::json!({
        "ops_checked": report.ops_checked,
        "bad_filename": report.bad_filename,
        "bad_json": report.bad_json,
        "bad_hash": bad_hash_json,
        "bad_op_shape": bad_shape_json,
        "tmp_total": report.tmp_total,
        "tmp_cleaned": report.tmp_cleaned,
    })
}

fn cmd_fsck(store_flag: Option<&Path>, json_mode: bool, clean_tmp: bool) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let report = fsck::run(&store, clean_tmp)?;

    if json_mode {
        let v = fsck_report_json(&report);
        println!("{}", serde_json::to_string(&v)?);
    } else {
        println!("ops checked: {}", report.ops_checked);
        if !report.bad_filename.is_empty() {
            println!("bad filenames: {}", report.bad_filename.len());
            for f in &report.bad_filename {
                println!("  {f}");
            }
        }
        if !report.bad_json.is_empty() {
            println!("bad json: {}", report.bad_json.len());
            for f in &report.bad_json {
                println!("  {f}");
            }
        }
        if !report.bad_hash.is_empty() {
            println!("bad hashes: {}", report.bad_hash.len());
            for (f, want, got) in &report.bad_hash {
                println!("  {f} expected {want} got {got}");
            }
        }
        if !report.bad_op_shape.is_empty() {
            println!("bad op shape: {}", report.bad_op_shape.len());
            for (f, err) in &report.bad_op_shape {
                println!("  {f}: {err}");
            }
        }
        let cleaned_msg = if clean_tmp {
            format!(" ({} cleaned)", report.tmp_cleaned)
        } else {
            String::new()
        };
        println!("tmp/: {} entries{}", report.tmp_total, cleaned_msg);
    }

    Ok(if report.is_clean() { 0 } else { 4 })
}

/// Test-only race-window widener. In debug/test builds, sleeps `<env var>`
/// milliseconds when the named env var is set. Used by integration tests to
/// widen compound command read-modify-write windows.
#[cfg(debug_assertions)]
fn maybe_test_sleep(var: &str) {
    if let Ok(s) = std::env::var(var) {
        if let Ok(ms) = s.parse::<u64>() {
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
    }
}

#[cfg(not(debug_assertions))]
fn maybe_test_sleep(_var: &str) {}

fn open_store(override_path: Option<&Path>) -> MoteResult<Store> {
    if let Some(p) = override_path {
        let candidate = if p.ends_with(".mote") {
            p.to_path_buf()
        } else {
            p.join(".mote")
        };
        if candidate.is_dir() {
            return Store::open(&candidate);
        }
        return Err(MoteError::StoreNotFound(p.to_path_buf()));
    }
    let cwd = std::env::current_dir()?;
    Store::discover(&cwd)
}

// Touch the `op` module path to silence the unused-import warning when the
// tagged-enum types are referenced only via re-exports.
#[allow(dead_code)]
fn _op_module_anchor() -> &'static str {
    op::VALID_NOTE_KINDS.first().copied().unwrap_or("note")
}
