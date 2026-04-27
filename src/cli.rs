//! Clap definitions and dispatcher.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use jiff::Timestamp;

use crate::errors::{MoteError, MoteResult};
use crate::ids;
use crate::op::{
    self, ScalarSet, Status, make_claim, make_close, make_create, make_delete, make_dep,
    make_msg_ack, make_msg_send, make_note, make_patch, make_release, make_reserve_close,
    make_reserve_open, make_tag, validate_msg_kind, validate_note_kind,
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

    /// Verify op-file hashes; with --clean-tmp, remove stale tmp/ entries
    Fsck {
        #[arg(long)]
        clean_tmp: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum DepCmd {
    /// Add a dependency edge: <child> is blocked by <parent>
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

pub fn run(cli: Cli) -> MoteResult<i32> {
    match cli.command {
        Command::Init => cmd_init(cli.quiet),
        Command::New {
            title,
            priority,
            body,
            assignee,
            tags,
            deps,
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
        } => cmd_note(cli.actor.as_deref(), cli.store.as_deref(), id, note_kind, text),
        Command::History {
            id,
            include_rejected,
        } => cmd_history(cli.store.as_deref(), cli.json, id, include_rejected),
        Command::Dep { cmd } => cmd_dep(cli.actor.as_deref(), cli.store.as_deref(), cmd),
        Command::Tag { cmd } => cmd_tag(cli.actor.as_deref(), cli.store.as_deref(), cmd),
        Command::Close { id } => cmd_close(cli.actor.as_deref(), cli.store.as_deref(), id),
        Command::Delete { id } => cmd_delete(cli.actor.as_deref(), cli.store.as_deref(), id),
        Command::Claim { id, ttl } => cmd_claim(cli.actor.as_deref(), cli.store.as_deref(), id, ttl),
        Command::Release { id } => cmd_release(cli.actor.as_deref(), cli.store.as_deref(), id),
        Command::Msg { cmd } => cmd_msg(cli.actor.as_deref(), cli.store.as_deref(), cmd),
        Command::Inbox { issue, from, kind } => {
            cmd_inbox(cli.actor.as_deref(), cli.store.as_deref(), cli.json, issue, from, kind)
        }
        Command::Reserve { paths, issue, ttl } => {
            cmd_reserve(cli.actor.as_deref(), cli.store.as_deref(), paths, issue, ttl)
        }
        Command::Unreserve { rv, paths } => {
            cmd_unreserve(cli.actor.as_deref(), cli.store.as_deref(), rv, paths)
        }
        Command::Preflight { issue, paths } => {
            cmd_preflight(cli.actor.as_deref(), cli.store.as_deref(), cli.json, issue, paths)
        }
        Command::Begin {
            id,
            paths,
            note,
            ttl,
        } => cmd_begin(cli.actor.as_deref(), cli.store.as_deref(), id, paths, note, ttl),
        Command::Handoff {
            id,
            to,
            note,
            release,
        } => cmd_handoff(cli.actor.as_deref(), cli.store.as_deref(), id, to, note, release),
        Command::Done { id, note } => {
            cmd_done(cli.actor.as_deref(), cli.store.as_deref(), id, note)
        }
        Command::WhoHas { path } => cmd_who_has(cli.store.as_deref(), cli.json, path),
        Command::Board => cmd_board(cli.actor.as_deref(), cli.store.as_deref(), cli.json),
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

    let bead_id = ids::new_bead_id();
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
        let op = make_tag(true, actor.clone(), bead_id.clone(), t.clone(), Timestamp::now());
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
            println!("tags:     {}", bead.tags.iter().cloned().collect::<Vec<_>>().join(", "));
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

fn cmd_dep(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    cmd: DepCmd,
) -> MoteResult<i32> {
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

fn cmd_tag(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    cmd: TagCmd,
) -> MoteResult<i32> {
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

    let op = make_claim(actor.clone(), id, actor, ttl_s, expect_claim, Timestamp::now());
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
            issue.as_deref().map_or(true, |i| m.entity.as_deref() == Some(i))
                && from.as_deref().map_or(true, |f| m.from == f)
                && kind.as_deref().map_or(true, |k| m.msg_kind == k)
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

    let issue_status = state.beads.get(&issue).map(|b| b.status.as_str().to_string());
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
    let note_op = make_note(actor.clone(), id.clone(), "note".into(), text, Timestamp::now());
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

fn cmd_who_has(
    store_flag: Option<&Path>,
    json_mode: bool,
    path: String,
) -> MoteResult<i32> {
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
        .filter(|b| b.claim.as_ref().map_or(false, |c| c.is_live(&now)))
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
            let holder = b.claim.as_ref().map(|c| c.claimed_by.as_str()).unwrap_or("?");
            println!("  {} ({}, by {holder})", b.id, b.status.as_str());
        }
        println!("reservations: {} active", active_reservations.len());
        for r in &active_reservations {
            let live = r.live_paths().join(", ");
            println!("  {} by {} on {}: {}", r.reservation_id, r.actor, r.entity, live);
        }
        println!("inbox:        {inbox_count} unacked");
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

fn cmd_fsck(store_flag: Option<&Path>, json_mode: bool, clean_tmp: bool) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let report = fsck::run(&store, clean_tmp)?;

    if json_mode {
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
        let v = serde_json::json!({
            "ops_checked": report.ops_checked,
            "bad_filename": report.bad_filename,
            "bad_json": report.bad_json,
            "bad_hash": bad_hash_json,
            "bad_op_shape": bad_shape_json,
            "tmp_total": report.tmp_total,
            "tmp_cleaned": report.tmp_cleaned,
        });
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
