//! Clap definitions and dispatcher.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::errors::{MoteError, MoteResult};
use crate::ids;
use crate::op::{
    self, ScalarSet, Status, make_board_post, make_board_read, make_board_route, make_board_sticky,
    make_board_topic, make_claim, make_close, make_create, make_delete, make_dep, make_msg_ack,
    make_msg_resolve, make_msg_send_with_metadata, make_note, make_patch, make_rel, make_release,
    make_reserve_close, make_reserve_open, make_session_end, make_session_start, make_tag,
    validate_msg_kind, validate_note_kind,
};
use crate::reducer;
use crate::state::{Bead, MsgRecord, RequestState};
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

    /// Override store path (then MOTE_STORE; default: discover from cwd)
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

    /// Manage actor identity and inspect actors observed in this store
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

    /// List non-blocking relation parents of a bead
    Parents { id: String },

    /// List non-blocking relation children of a bead
    Children { id: String },

    /// List beads blocked by this bead
    Dependents { id: String },

    /// List beads
    Ls {
        /// Filter by status (open|doing|blocked|review|closed)
        #[arg(long)]
        status: Option<String>,
        /// Filter by tag. Repeat for intersection, e.g. --tag m1 --tag task.
        #[arg(long = "tag")]
        tags: Vec<String>,
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

    /// Manage non-blocking relationships
    Rel {
        #[command(subcommand)]
        cmd: RelCmd,
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
        /// Stream existing unacked messages, then new deliveries
        #[arg(long, conflicts_with = "wait")]
        follow: bool,
        /// Return pending messages, or wait for the next delivery and exit
        #[arg(long, conflicts_with = "follow")]
        wait: bool,
        /// Maximum seconds to wait (default: 60; requires --wait)
        #[arg(long, requires = "wait")]
        timeout: Option<u64>,
        /// Resume follow mode after an event/op id instead of emitting current inbox state
        #[arg(long)]
        after: Option<String>,
        /// Periodic fallback scan interval in seconds
        #[arg(long, default_value_t = 5)]
        interval: u64,
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

    /// Compound: reserve_open + claim + status=doing + optional progress note. Compensates on partial failure.
    Begin {
        id: String,
        #[arg(long = "paths", num_args = 1..)]
        paths: Vec<String>,
        #[arg(long)]
        note: Option<String>,
        #[arg(long)]
        ttl: Option<u32>,
        /// Also post a one-line claim to this discussion topic
        #[arg(long)]
        announce: Option<String>,
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

    /// Manage per-session identity so concurrent sessions stay distinguishable
    Session {
        #[command(subcommand)]
        cmd: SessionCmd,
    },

    /// One-shot view of what is actively being worked on right now
    InFlight {
        /// Treat discussion topics touched within this many minutes as active
        #[arg(long, default_value_t = 60)]
        minutes: u64,
        /// Omit the advisory recent-commit section
        #[arg(long)]
        no_git: bool,
    },

    /// Compact overview of board state
    Board,

    /// Emit accepted operation events, optionally following for new events
    Events {
        /// Event categories: issue, claim, reservation, message, discussion, or all
        #[arg(long = "kind", value_delimiter = ',')]
        kinds: Vec<String>,
        /// Include only events authored by or directly related to this actor
        #[arg(long = "for-actor")]
        for_actor: Option<String>,
        /// Emit only events after this event/op id
        #[arg(long)]
        after: Option<String>,
        /// Continue waiting for new events
        #[arg(long)]
        follow: bool,
        /// Periodic fallback scan interval in seconds
        #[arg(long, default_value_t = 5)]
        interval: u64,
    },

    /// Stream snapshots whenever the op log changes. Read-only.
    Watch {
        /// Periodic re-emit interval in seconds (fallback to the FS watcher)
        #[arg(long, default_value_t = 5)]
        interval: u64,
    },

    /// Open a read-only TUI dashboard. Read-only.
    Ui,

    /// Check store layout, actor identity, and op-log health
    Doctor,

    /// Verify op-file hashes; with --clean-tmp, remove stale tmp/ entries
    Fsck {
        #[arg(long)]
        clean_tmp: bool,
    },

    /// Publish a JSONL batch of ordinary mote operations sequentially
    Batch {
        /// Input file, or stdin when omitted / "-"
        input: Option<PathBuf>,
    },

    /// Import a JSON plan containing beads, deps, and relations
    Import {
        /// Input file, or stdin when omitted / "-"
        input: Option<PathBuf>,
    },

    /// Manage canonical mote skills bundled with this binary
    Skills {
        #[command(subcommand)]
        cmd: SkillsCmd,
    },
}

#[derive(Subcommand, Debug)]
pub enum SkillsCmd {
    /// List skills bundled with this mote binary
    List,
    /// Install bundled skills into a user-global or repo-local skills directory
    Install {
        /// Install for the current user (~/.claude/skills and ~/.codex/skills)
        #[arg(long, conflicts_with = "repo")]
        user: bool,
        /// Install into a target repository's `.claude/skills` and `.codex/skills`
        #[arg(long, conflicts_with = "user")]
        repo: Option<PathBuf>,
        /// Comma-separated agent list (default: claude,codex)
        #[arg(long = "agent", value_delimiter = ',')]
        agents: Vec<String>,
        /// Overwrite existing skill directories
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum SessionCmd {
    /// Open a session lease and print the `export MOTE_ACTOR=...` line to activate it
    Start {
        /// Actor identity for this session (default: the resolved actor)
        #[arg(long = "as")]
        as_actor: Option<String>,
        /// Lease duration in seconds
        #[arg(long, default_value_t = 14400)]
        ttl: u32,
        /// Free-text description of what this session is doing
        #[arg(long)]
        label: Option<String>,
    },
    /// Extend an existing session lease
    Renew {
        /// Session id (default: `MOTE_SESSION`)
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        ttl: Option<u32>,
    },
    /// List session leases
    List {
        /// Include ended and expired sessions
        #[arg(long)]
        all: bool,
    },
    /// End a session lease
    End {
        /// Session id (default: `MOTE_SESSION`)
        id: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum ActorCmd {
    /// Persist an actor identity in `.mote/local/actor`
    Set { actor: String },
    /// Show the actor identity that would be used for this invocation
    Show,
    /// List actors observed in accepted operations or message recipients
    List,
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
pub enum RelCmd {
    /// Add a non-blocking relationship edge: `child` is contained by `parent`
    Add {
        child: String,
        parent: String,
        #[arg(long, default_value = "parent")]
        kind: String,
    },
    /// Remove a non-blocking relationship edge
    Rm {
        child: String,
        parent: String,
        #[arg(long, default_value = "parent")]
        kind: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum TagCmd {
    Add {
        id: String,
        #[arg(num_args = 1.., required = true)]
        tags: Vec<String>,
    },
    Rm {
        id: String,
        tag: String,
    },
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
        /// Sender-scoped retry key; an identical retry returns the first msg-id
        #[arg(long)]
        idempotency_key: Option<String>,
        /// Body text (positional)
        text: String,
    },
    /// Respond to or decline an open request
    Reply {
        /// Root request msg-id
        msg_id: String,
        /// Reply kind (response | decline)
        #[arg(long = "kind", default_value = "response")]
        msg_kind: String,
        /// Sender-scoped retry key; an identical retry returns the first msg-id
        #[arg(long)]
        idempotency_key: Option<String>,
        /// Body text (positional)
        text: String,
    },
    /// List request lifecycles involving the current actor
    Requests {
        /// Filter by state (open | responded | declined | resolved)
        #[arg(long = "state")]
        request_state: Option<String>,
    },
    /// Mark a responded or declined request resolved (request sender only)
    Resolve {
        /// Root request msg-id
        msg_id: String,
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
        /// Body text
        #[arg(long)]
        body: Option<String>,
        /// Body text (positional; alternatively use --body)
        text: Option<String>,
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
    /// Record a decision on a topic as a sticky, retrievable post
    Decision {
        #[arg(long, default_value = "general")]
        topic: String,
        /// Decision text
        #[arg(long)]
        body: Option<String>,
        /// Decision text (positional; alternatively use --body)
        text: Option<String>,
    },
    /// Set or show a topic's pinned current-state summary
    Summary {
        #[arg(long, default_value = "general")]
        topic: String,
        /// Summary text; omit to print the topic's current summary
        #[arg(long)]
        body: Option<String>,
        /// Summary text (positional; alternatively use --body)
        text: Option<String>,
    },
    /// Link a post or topic to a bead
    Route {
        /// Post to route (omit and pass --topic to route a whole topic)
        post_id: Option<String>,
        #[arg(long, conflicts_with = "post_id")]
        topic: Option<String>,
        /// Bead this discussion routes to
        #[arg(long = "issue")]
        issue: String,
        /// Optional note recorded on the bead
        #[arg(long)]
        note: Option<String>,
    },
    /// Flag a post or topic as actionable but not yet routed to a bead
    NeedsBead {
        post_id: Option<String>,
        #[arg(long, conflicts_with = "post_id")]
        topic: Option<String>,
    },
    /// Mark a post or topic as no longer needing tracker action
    Resolve {
        post_id: Option<String>,
        #[arg(long, conflicts_with = "post_id")]
        topic: Option<String>,
    },
    /// List discussion flagged as needing tracker action
    Unrouted {
        #[arg(long)]
        topic: Option<String>,
    },
    /// Create a bead from a post and route the post to it
    Promote {
        post_id: String,
        /// Bead title (default: the post's first line)
        #[arg(long)]
        title: Option<String>,
        /// Bead body (default: the post body, with board provenance appended)
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        priority: Option<i32>,
        #[arg(long = "tag", value_delimiter = ',')]
        tags: Vec<String>,
        #[arg(long = "dep", value_delimiter = ',')]
        deps: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum DiscussTopicCmd {
    /// Create a topic before any posts exist
    New {
        topic: String,
        /// Display title (defaults to topic)
        #[arg(long)]
        title: Option<String>,
        /// Topic description shown in topic listings
        #[arg(long)]
        description: Option<String>,
        /// Optional initial visible post body
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
        Command::Parents { id } => cmd_parents(cli.store.as_deref(), cli.json, id),
        Command::Children { id } => cmd_children(cli.store.as_deref(), cli.json, id),
        Command::Dependents { id } => cmd_dependents(cli.store.as_deref(), cli.json, id),
        Command::Ls {
            status,
            tags,
            assignee,
            all,
            ready,
        } => cmd_ls(
            cli.actor.as_deref(),
            cli.store.as_deref(),
            cli.json,
            status,
            tags,
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
        Command::Dep { cmd } => cmd_dep(cli.actor.as_deref(), cli.store.as_deref(), cli.quiet, cmd),
        Command::Rel { cmd } => cmd_rel(cli.actor.as_deref(), cli.store.as_deref(), cmd),
        Command::Tag { cmd } => cmd_tag(cli.actor.as_deref(), cli.store.as_deref(), cmd),
        Command::Close { id } => cmd_close(cli.actor.as_deref(), cli.store.as_deref(), id),
        Command::Delete { id } => cmd_delete(cli.actor.as_deref(), cli.store.as_deref(), id),
        Command::Claim { id, ttl } => {
            cmd_claim(cli.actor.as_deref(), cli.store.as_deref(), id, ttl)
        }
        Command::Release { id } => cmd_release(cli.actor.as_deref(), cli.store.as_deref(), id),
        Command::Msg { cmd } => cmd_msg(cli.actor.as_deref(), cli.store.as_deref(), cli.json, cmd),
        Command::Discuss { cmd } => {
            cmd_discuss(cli.actor.as_deref(), cli.store.as_deref(), cli.json, cmd)
        }
        Command::Inbox {
            issue,
            from,
            kind,
            follow,
            wait,
            timeout,
            after,
            interval,
        } => cmd_inbox(
            cli.actor.as_deref(),
            cli.store.as_deref(),
            cli.json,
            issue,
            from,
            kind,
            follow,
            wait,
            timeout,
            after,
            interval,
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
            announce,
        } => cmd_begin(
            cli.actor.as_deref(),
            cli.store.as_deref(),
            id,
            paths,
            note,
            ttl,
            announce,
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
        Command::Session { cmd } => {
            cmd_session(cli.actor.as_deref(), cli.store.as_deref(), cli.json, cmd)
        }
        Command::InFlight { minutes, no_git } => cmd_in_flight(
            cli.actor.as_deref(),
            cli.store.as_deref(),
            cli.json,
            minutes,
            !no_git,
        ),
        Command::Board => cmd_board(cli.actor.as_deref(), cli.store.as_deref(), cli.json),
        Command::Events {
            kinds,
            for_actor,
            after,
            follow,
            interval,
        } => cmd_events(
            cli.actor.as_deref(),
            cli.store.as_deref(),
            cli.json,
            kinds,
            for_actor,
            after,
            follow,
            interval,
        ),
        Command::Watch { interval } => cmd_watch(
            cli.actor.as_deref(),
            cli.store.as_deref(),
            cli.json,
            interval,
        ),
        Command::Ui => cmd_ui(cli.actor.as_deref(), cli.store.as_deref()),
        Command::Doctor => cmd_doctor(cli.actor.as_deref(), cli.store.as_deref(), cli.json),
        Command::Fsck { clean_tmp } => cmd_fsck(cli.store.as_deref(), cli.json, clean_tmp),
        Command::Batch { input } => {
            cmd_batch(cli.actor.as_deref(), cli.store.as_deref(), cli.json, input)
        }
        Command::Import { input } => {
            cmd_import(cli.actor.as_deref(), cli.store.as_deref(), cli.json, input)
        }
        Command::Skills { cmd } => cmd_skills(cli.json, cli.quiet, cmd),
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
        ActorCmd::List => {
            let state = reducer::replay_store(&store)?;
            let current = store.resolve_actor(actor_flag).ok();
            let actors = actor_summaries(&state, current.as_deref());
            if json_mode {
                println!("{}", serde_json::to_string(&actors)?);
            } else {
                for actor in actors {
                    let marker = if actor.current { "*" } else { " " };
                    println!(
                        "{marker} {}  last={}  claims={}  reservations={}  inbox={}  open-requests={}",
                        actor.actor,
                        actor.last_activity_ts.as_deref().unwrap_or("-"),
                        actor.active_claims,
                        actor.active_reservations,
                        actor.inbox_unacked,
                        actor.incoming_open_requests,
                    );
                }
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

#[derive(Debug, Default)]
struct ActorActivity {
    last_activity_ts: Option<String>,
    last_activity_op_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct ActorSummary {
    actor: String,
    current: bool,
    last_activity_ts: Option<String>,
    last_activity_op_id: Option<String>,
    active_claims: usize,
    active_reservations: usize,
    inbox_unacked: usize,
    incoming_open_requests: usize,
}

fn actor_summaries(state: &crate::state::State, current: Option<&str>) -> Vec<ActorSummary> {
    let mut activity: BTreeMap<String, ActorActivity> = BTreeMap::new();
    if let Some(actor) = current {
        activity.entry(actor.to_string()).or_default();
    }

    for entry in state
        .history
        .values()
        .flatten()
        .chain(state.orphan_history.iter())
        .filter(|entry| entry.accepted && entry.actor != "?")
    {
        let actor = activity.entry(entry.actor.clone()).or_default();
        if actor
            .last_activity_op_id
            .as_deref()
            .is_none_or(|op_id| entry.op_id.as_str() > op_id)
        {
            actor.last_activity_ts = Some(entry.ts.clone());
            actor.last_activity_op_id = Some(entry.op_id.clone());
        }
    }

    for message in state.messages.values() {
        activity.entry(message.from.clone()).or_default();
        activity.entry(message.to.clone()).or_default();
    }
    for bead in state.beads.values() {
        if let Some(claim) = &bead.claim {
            activity.entry(claim.claimed_by.clone()).or_default();
        }
    }
    for reservation in state.reservations.values() {
        activity.entry(reservation.actor.clone()).or_default();
    }

    let now = ids::format_rfc3339(Timestamp::now());
    activity
        .into_iter()
        .map(|(name, activity)| ActorSummary {
            current: current == Some(name.as_str()),
            active_claims: state
                .beads
                .values()
                .filter(|bead| {
                    bead.claim
                        .as_ref()
                        .is_some_and(|claim| claim.claimed_by == name && claim.is_live(&now))
                })
                .count(),
            active_reservations: state
                .reservations
                .values()
                .filter(|reservation| reservation.actor == name && reservation.is_active(&now))
                .count(),
            inbox_unacked: state.inbox_for(&name).len(),
            incoming_open_requests: state
                .messages
                .values()
                .filter(|message| {
                    message.to == name && message.request_state == Some(RequestState::Open)
                })
                .count(),
            actor: name,
            last_activity_ts: activity.last_activity_ts,
            last_activity_op_id: activity.last_activity_op_id,
        })
        .collect()
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

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum EdgeSpec {
    Id(String),
    Object {
        parent: String,
        #[serde(default)]
        kind: Option<String>,
    },
}

impl EdgeSpec {
    fn into_parent_kind(self, default_kind: &str) -> (String, String) {
        match self {
            EdgeSpec::Id(parent) => (parent, default_kind.to_string()),
            EdgeSpec::Object { parent, kind } => {
                (parent, kind.unwrap_or_else(|| default_kind.to_string()))
            }
        }
    }
}

#[derive(Debug)]
struct NewSpec {
    id: Option<String>,
    title: String,
    priority: Option<i32>,
    body: Option<String>,
    assignee: Option<String>,
    tags: Vec<String>,
    deps: Vec<EdgeSpec>,
    relations: Vec<EdgeSpec>,
}

#[derive(Debug, Deserialize)]
struct BatchItem {
    action: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default, alias = "entity")]
    child: Option<String>,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    priority: Option<i32>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    deps: Vec<EdgeSpec>,
    #[serde(default, alias = "rels")]
    relations: Vec<EdgeSpec>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    note_kind: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ImportPlan {
    #[serde(default)]
    beads: Vec<ImportBead>,
    #[serde(default)]
    deps: Vec<ImportEdge>,
    #[serde(default, alias = "rels")]
    relations: Vec<ImportEdge>,
}

#[derive(Debug, Deserialize)]
struct ImportBead {
    #[serde(default)]
    id: Option<String>,
    title: String,
    #[serde(default)]
    priority: Option<i32>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    deps: Vec<EdgeSpec>,
    #[serde(default, alias = "rels")]
    relations: Vec<EdgeSpec>,
}

#[derive(Debug, Deserialize)]
struct ImportEdge {
    #[serde(alias = "entity")]
    child: String,
    parent: String,
    #[serde(default)]
    kind: Option<String>,
}

#[derive(Debug, Serialize)]
struct BatchOutcome {
    action: String,
    entity: Option<String>,
    parent: Option<String>,
    kind: Option<String>,
    op_id: Option<String>,
    status: String,
    reason: Option<String>,
}

fn cmd_batch(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    json_mode: bool,
    input: Option<PathBuf>,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;
    let content = read_command_input(input)?;
    let mut outcomes = Vec::new();

    for (idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let item: BatchItem = serde_json::from_str(trimmed)
            .map_err(|e| MoteError::Invalid(format!("batch line {}: {e}", idx + 1)))?;
        process_batch_item(&store, &actor, item, &mut outcomes)?;
    }

    print_batch_report(&outcomes, json_mode)
}

fn cmd_import(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    json_mode: bool,
    input: Option<PathBuf>,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;
    let content = read_command_input(input)?;
    let plan: ImportPlan = serde_json::from_str(&content)
        .map_err(|e| MoteError::Invalid(format!("import JSON: {e}")))?;
    let mut outcomes = Vec::new();

    for bead in plan.beads {
        let spec = NewSpec {
            id: bead.id,
            title: bead.title,
            priority: bead.priority,
            body: bead.body,
            assignee: bead.assignee,
            tags: bead.tags,
            deps: bead.deps,
            relations: bead.relations,
        };
        let _ = publish_new_spec(&store, &actor, spec, &mut outcomes)?;
    }
    for dep in plan.deps {
        publish_edge(
            &store,
            &actor,
            true,
            dep.child,
            dep.parent,
            dep.kind.unwrap_or_else(|| "blocks".to_string()),
            true,
            &mut outcomes,
        )?;
    }
    for rel in plan.relations {
        publish_edge(
            &store,
            &actor,
            true,
            rel.child,
            rel.parent,
            rel.kind.unwrap_or_else(|| "parent".to_string()),
            false,
            &mut outcomes,
        )?;
    }

    print_batch_report(&outcomes, json_mode)
}

fn read_command_input(input: Option<PathBuf>) -> MoteResult<String> {
    match input {
        Some(path) if path.as_os_str() != "-" => Ok(fs::read_to_string(path)?),
        _ => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            Ok(s)
        }
    }
}

fn process_batch_item(
    store: &Store,
    actor: &str,
    item: BatchItem,
    outcomes: &mut Vec<BatchOutcome>,
) -> MoteResult<()> {
    match item.action.as_str() {
        "create" | "new" => {
            let title = item
                .title
                .ok_or_else(|| MoteError::Invalid("batch create requires title".into()))?;
            let spec = NewSpec {
                id: item.id,
                title,
                priority: item.priority,
                body: item.body,
                assignee: item.assignee,
                tags: item.tags,
                deps: item.deps,
                relations: item.relations,
            };
            let _ = publish_new_spec(store, actor, spec, outcomes)?;
        }
        "tag_add" => {
            let id = require_item_id(&item, "tag_add")?;
            let tags = item_tags(item)?;
            for tag in tags {
                publish_tag(store, actor, true, id.clone(), tag, outcomes)?;
            }
        }
        "tag_remove" | "tag_rm" => {
            let id = require_item_id(&item, "tag_remove")?;
            let tags = item_tags(item)?;
            for tag in tags {
                publish_tag(store, actor, false, id.clone(), tag, outcomes)?;
            }
        }
        "dep_add" => {
            let child = require_child_id(&item, "dep_add")?;
            let parent = require_parent_id(&item, "dep_add")?;
            let kind = item.kind.unwrap_or_else(|| "blocks".to_string());
            publish_edge(store, actor, true, child, parent, kind, true, outcomes)?;
        }
        "dep_remove" | "dep_rm" => {
            let child = require_child_id(&item, "dep_remove")?;
            let parent = require_parent_id(&item, "dep_remove")?;
            let kind = item.kind.unwrap_or_else(|| "blocks".to_string());
            publish_edge(store, actor, false, child, parent, kind, true, outcomes)?;
        }
        "rel_add" => {
            let child = require_child_id(&item, "rel_add")?;
            let parent = require_parent_id(&item, "rel_add")?;
            let kind = item.kind.unwrap_or_else(|| "parent".to_string());
            publish_edge(store, actor, true, child, parent, kind, false, outcomes)?;
        }
        "rel_remove" | "rel_rm" => {
            let child = require_child_id(&item, "rel_remove")?;
            let parent = require_parent_id(&item, "rel_remove")?;
            let kind = item.kind.unwrap_or_else(|| "parent".to_string());
            publish_edge(store, actor, false, child, parent, kind, false, outcomes)?;
        }
        "note" => {
            let id = require_item_id(&item, "note")?;
            let note_kind = item.note_kind.unwrap_or_else(|| "note".to_string());
            let text = item
                .text
                .ok_or_else(|| MoteError::Invalid("batch note requires text".into()))?;
            if !validate_note_kind(&note_kind) {
                return Err(MoteError::Invalid(format!(
                    "invalid note_kind `{note_kind}` (expected one of: {})",
                    op::VALID_NOTE_KINDS.join(" | ")
                )));
            }
            let op = make_note(
                actor.to_string(),
                id.clone(),
                note_kind,
                text,
                Timestamp::now(),
            );
            record_published_op(store, &op, "note", Some(id), None, None, outcomes)?;
        }
        other => {
            return Err(MoteError::Invalid(format!(
                "unknown batch action `{other}`"
            )));
        }
    }
    Ok(())
}

fn require_item_id(item: &BatchItem, action: &str) -> MoteResult<String> {
    item.id
        .clone()
        .ok_or_else(|| MoteError::Invalid(format!("batch {action} requires id")))
}

fn require_child_id(item: &BatchItem, action: &str) -> MoteResult<String> {
    item.child
        .clone()
        .or_else(|| item.id.clone())
        .ok_or_else(|| MoteError::Invalid(format!("batch {action} requires child or entity")))
}

fn require_parent_id(item: &BatchItem, action: &str) -> MoteResult<String> {
    item.parent
        .clone()
        .ok_or_else(|| MoteError::Invalid(format!("batch {action} requires parent")))
}

fn item_tags(mut item: BatchItem) -> MoteResult<Vec<String>> {
    if let Some(tag) = item.tag.take() {
        item.tags.insert(0, tag);
    }
    if item.tags.is_empty() {
        return Err(MoteError::Invalid(
            "batch tag action requires tag or tags".into(),
        ));
    }
    Ok(item.tags)
}

fn publish_new_spec(
    store: &Store,
    actor: &str,
    spec: NewSpec,
    outcomes: &mut Vec<BatchOutcome>,
) -> MoteResult<Option<String>> {
    if spec.title.trim().is_empty() {
        return Err(MoteError::Invalid(
            "batch create title must be non-empty".into(),
        ));
    }
    if let Some(p) = spec.priority {
        if !(0..=3).contains(&p) {
            return Err(MoteError::Invalid(format!("priority {p} out of 0..=3")));
        }
    }

    let bead_id = match spec.id {
        Some(custom) => {
            ids::validate_external_bead_id(&custom)?;
            custom
        }
        None => ids::new_bead_id(),
    };
    let mut set = ScalarSet {
        title: Some(spec.title),
        priority: spec.priority,
        body: spec.body,
        assignee: spec.assignee,
        ..Default::default()
    };
    set.status = Some(Status::Open);

    let create = make_create(actor.to_string(), bead_id.clone(), set, Timestamp::now());
    let accepted = record_published_op(
        store,
        &create,
        "create",
        Some(bead_id.clone()),
        None,
        None,
        outcomes,
    )?;
    if !accepted {
        record_skip(
            "create_children",
            Some(bead_id.clone()),
            None,
            None,
            "create rejected",
            outcomes,
        );
        return Ok(None);
    }

    for tag in spec.tags {
        publish_tag(store, actor, true, bead_id.clone(), tag, outcomes)?;
    }
    for dep in spec.deps {
        let (parent, kind) = dep.into_parent_kind("blocks");
        publish_edge(
            store,
            actor,
            true,
            bead_id.clone(),
            parent,
            kind,
            true,
            outcomes,
        )?;
    }
    for rel in spec.relations {
        let (parent, kind) = rel.into_parent_kind("parent");
        publish_edge(
            store,
            actor,
            true,
            bead_id.clone(),
            parent,
            kind,
            false,
            outcomes,
        )?;
    }

    Ok(Some(bead_id))
}

fn publish_tag(
    store: &Store,
    actor: &str,
    add: bool,
    id: String,
    tag: String,
    outcomes: &mut Vec<BatchOutcome>,
) -> MoteResult<bool> {
    let op = make_tag(
        add,
        actor.to_string(),
        id.clone(),
        tag.clone(),
        Timestamp::now(),
    );
    record_published_op(
        store,
        &op,
        if add { "tag_add" } else { "tag_remove" },
        Some(id),
        None,
        Some(tag),
        outcomes,
    )
}

#[allow(clippy::too_many_arguments)]
fn publish_edge(
    store: &Store,
    actor: &str,
    add: bool,
    child: String,
    parent: String,
    edge_kind: String,
    blocking: bool,
    outcomes: &mut Vec<BatchOutcome>,
) -> MoteResult<bool> {
    let (op, action) = if blocking {
        (
            make_dep(
                add,
                actor.to_string(),
                child.clone(),
                parent.clone(),
                edge_kind.clone(),
                Timestamp::now(),
            ),
            if add { "dep_add" } else { "dep_remove" },
        )
    } else {
        (
            make_rel(
                add,
                actor.to_string(),
                child.clone(),
                parent.clone(),
                edge_kind.clone(),
                Timestamp::now(),
            ),
            if add { "rel_add" } else { "rel_remove" },
        )
    };
    record_published_op(
        store,
        &op,
        action,
        Some(child),
        Some(parent),
        Some(edge_kind),
        outcomes,
    )
}

fn record_published_op(
    store: &Store,
    op: &op::Op,
    action: &str,
    entity: Option<String>,
    parent: Option<String>,
    kind: Option<String>,
    outcomes: &mut Vec<BatchOutcome>,
) -> MoteResult<bool> {
    let name = publish::publish_op(store, op)?;
    let state = reducer::replay_store(store)?;
    let op_id = name.as_str().to_string();
    let accepted = state.was_accepted(&op_id);
    let reason = if accepted {
        None
    } else {
        state
            .rejection_reason(&op_id)
            .or_else(|| Some("unknown".into()))
    };
    outcomes.push(BatchOutcome {
        action: action.to_string(),
        entity,
        parent,
        kind,
        op_id: Some(op_id),
        status: if accepted { "accepted" } else { "rejected" }.to_string(),
        reason,
    });
    Ok(accepted)
}

fn record_skip(
    action: &str,
    entity: Option<String>,
    parent: Option<String>,
    kind: Option<&str>,
    reason: &str,
    outcomes: &mut Vec<BatchOutcome>,
) {
    outcomes.push(BatchOutcome {
        action: action.to_string(),
        entity,
        parent,
        kind: kind.map(str::to_string),
        op_id: None,
        status: "skipped".to_string(),
        reason: Some(reason.to_string()),
    });
}

fn print_batch_report(outcomes: &[BatchOutcome], json_mode: bool) -> MoteResult<i32> {
    let accepted = outcomes.iter().filter(|o| o.status == "accepted").count();
    let rejected = outcomes.iter().filter(|o| o.status == "rejected").count();
    let skipped = outcomes.iter().filter(|o| o.status == "skipped").count();
    if json_mode {
        let v = serde_json::json!({
            "accepted": accepted,
            "rejected": rejected,
            "skipped": skipped,
            "results": outcomes,
        });
        println!("{}", serde_json::to_string(&v)?);
    } else {
        for outcome in outcomes {
            let entity = outcome.entity.as_deref().unwrap_or("-");
            let parent = outcome.parent.as_deref().unwrap_or("-");
            let kind = outcome.kind.as_deref().unwrap_or("-");
            let op_id = outcome.op_id.as_deref().unwrap_or("-");
            match outcome.reason.as_deref() {
                Some(reason) => println!(
                    "{} {} entity={} parent={} kind={} op={} reason={}",
                    outcome.status, outcome.action, entity, parent, kind, op_id, reason
                ),
                None => println!(
                    "{} {} entity={} parent={} kind={} op={}",
                    outcome.status, outcome.action, entity, parent, kind, op_id
                ),
            }
        }
    }
    Ok(if rejected > 0 || skipped > 0 { 2 } else { 0 })
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
            "relations": bead.rels.iter().map(|(p, k)| serde_json::json!({"parent": p, "kind": k})).collect::<Vec<_>>(),
            "children": state.relation_children_of(&id).iter().map(|(b, k)| bead_edge_json(b, k)).collect::<Vec<_>>(),
            "dependents": state.dependency_children_of(&id).iter().map(|(b, k)| bead_edge_json(b, k)).collect::<Vec<_>>(),
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
        if !bead.rels.is_empty() {
            print!("rels:     ");
            let parts: Vec<String> = bead
                .rels
                .iter()
                .map(|(p, k)| format!("{p} ({k})"))
                .collect();
            println!("{}", parts.join(", "));
        }
        let children = state.relation_children_of(&id);
        if !children.is_empty() {
            print!("children: ");
            let parts: Vec<String> = children
                .iter()
                .map(|(b, k)| format!("{} ({k})", b.id))
                .collect();
            println!("{}", parts.join(", "));
        }
        let dependents = state.dependency_children_of(&id);
        if !dependents.is_empty() {
            print!("dependents: ");
            let parts: Vec<String> = dependents
                .iter()
                .map(|(b, k)| format!("{} ({k})", b.id))
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

fn bead_edge_json(bead: &Bead, kind: &str) -> serde_json::Value {
    serde_json::json!({
        "id": bead.id,
        "title": bead.title,
        "status": bead.status.as_str(),
        "priority": bead.priority,
        "kind": kind,
        "tags": bead.tags.iter().collect::<Vec<_>>(),
        "assignee": bead.assignee,
    })
}

fn cmd_parents(store_flag: Option<&Path>, json_mode: bool, id: String) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let state = reducer::replay_store(&store)?;
    let bead = state
        .beads
        .get(&id)
        .ok_or_else(|| MoteError::Invalid(format!("no such bead {id}")))?;

    if json_mode {
        let arr: Vec<_> = bead
            .rels
            .iter()
            .map(|(parent, kind)| {
                let parent_bead = state.beads.get(parent);
                serde_json::json!({
                    "id": parent,
                    "kind": kind,
                    "title": parent_bead.map(|b| b.title.as_str()),
                    "status": parent_bead.map(|b| b.status.as_str()),
                    "priority": parent_bead.map(|b| b.priority),
                    "deleted": parent_bead.is_some_and(|b| b.is_deleted()),
                })
            })
            .collect();
        println!("{}", serde_json::to_string(&arr)?);
    } else {
        for (parent, kind) in &bead.rels {
            match state.beads.get(parent) {
                Some(b) => println!(
                    "{:<24} ({:<8}) p{} {:<8} {}",
                    parent,
                    kind,
                    b.priority,
                    b.status.as_str(),
                    b.title
                ),
                None => println!("{:<24} ({kind})", parent),
            }
        }
    }
    Ok(0)
}

fn cmd_children(store_flag: Option<&Path>, json_mode: bool, id: String) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let state = reducer::replay_store(&store)?;
    if !state.beads.contains_key(&id) {
        return Err(MoteError::Invalid(format!("no such bead {id}")));
    }
    let children = state.relation_children_of(&id);
    print_edge_beads(children, json_mode)
}

fn cmd_dependents(store_flag: Option<&Path>, json_mode: bool, id: String) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let state = reducer::replay_store(&store)?;
    if !state.beads.contains_key(&id) {
        return Err(MoteError::Invalid(format!("no such bead {id}")));
    }
    let dependents = state.dependency_children_of(&id);
    print_edge_beads(dependents, json_mode)
}

fn print_edge_beads(beads: Vec<(&Bead, &str)>, json_mode: bool) -> MoteResult<i32> {
    if json_mode {
        let arr: Vec<_> = beads.iter().map(|(b, k)| bead_edge_json(b, k)).collect();
        println!("{}", serde_json::to_string(&arr)?);
    } else {
        for (b, kind) in beads {
            println!(
                "{:<24} ({:<8}) p{} {:<8} {}",
                b.id,
                kind,
                b.priority,
                b.status.as_str(),
                b.title
            );
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
    tags: Vec<String>,
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
            for t in &tags {
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
    quiet: bool,
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
    if !quiet && looks_like_containment_dep_kind(&kind) {
        let rel_cmd = if add { "add" } else { "rm" };
        eprintln!(
            "warning: dep --kind {kind} is still a blocking dependency; use `mote rel {rel_cmd} --kind {kind}` for non-blocking containment"
        );
    }
    let op = make_dep(add, actor, child, parent, kind, Timestamp::now());
    let name = publish::publish_op(&store, &op)?;
    verify_accept(&store, &name)
}

fn looks_like_containment_dep_kind(kind: &str) -> bool {
    matches!(
        kind,
        "parent"
            | "child"
            | "children"
            | "subtask"
            | "sub-task"
            | "epic"
            | "contains"
            | "containment"
            | "hierarchy"
    )
}

fn cmd_rel(actor_flag: Option<&str>, store_flag: Option<&Path>, cmd: RelCmd) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;
    let (add, child, parent, kind) = match cmd {
        RelCmd::Add {
            child,
            parent,
            kind,
        } => (true, child, parent, kind),
        RelCmd::Rm {
            child,
            parent,
            kind,
        } => (false, child, parent, kind),
    };
    let op = make_rel(add, actor, child, parent, kind, Timestamp::now());
    let name = publish::publish_op(&store, &op)?;
    verify_accept(&store, &name)
}

fn cmd_tag(actor_flag: Option<&str>, store_flag: Option<&Path>, cmd: TagCmd) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;
    let (add, id, tags) = match cmd {
        TagCmd::Add { id, tags } => (true, id, tags),
        TagCmd::Rm { id, tag } => (false, id, vec![tag]),
    };
    let mut had_failure = false;
    for tag in tags {
        let op = make_tag(add, actor.clone(), id.clone(), tag, Timestamp::now());
        let name = publish::publish_op(&store, &op)?;
        if verify_accept(&store, &name)? != 0 {
            had_failure = true;
        }
    }
    Ok(if had_failure { 2 } else { 0 })
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

#[derive(Clone)]
struct MessageDraft {
    to: String,
    entity: Option<String>,
    reservation: Option<String>,
    msg_kind: String,
    body: String,
    reply_to: Option<String>,
    correlation_id: Option<String>,
    idempotency_key: Option<String>,
}

fn message_matches_draft(message: &MsgRecord, draft: &MessageDraft) -> bool {
    let correlation_matches = match draft.correlation_id.as_deref() {
        Some(correlation) => message.correlation_id.as_deref() == Some(correlation),
        None if draft.msg_kind == "request" => true,
        None => message.correlation_id.is_none(),
    };
    message.to == draft.to
        && message.entity == draft.entity
        && message.reservation == draft.reservation
        && message.msg_kind == draft.msg_kind
        && message.body == draft.body
        && message.reply_to == draft.reply_to
        && correlation_matches
}

fn existing_idempotent_message(
    state: &crate::state::State,
    actor: &str,
    draft: &MessageDraft,
) -> MoteResult<Option<String>> {
    let Some(key) = draft.idempotency_key.as_deref() else {
        return Ok(None);
    };
    if !op::validate_idempotency_key(key) {
        return Err(MoteError::Invalid(
            "--idempotency-key must be 1..=128 trimmed printable characters".into(),
        ));
    }
    let Some(existing) = state.message_by_idempotency(actor, key) else {
        return Ok(None);
    };
    if message_matches_draft(existing, draft) {
        Ok(Some(existing.msg_id.clone()))
    } else {
        Err(MoteError::Invalid(format!(
            "idempotency key `{key}` is already used by {} with different message content",
            existing.msg_id
        )))
    }
}

fn publish_message(store: &Store, actor: String, draft: MessageDraft) -> MoteResult<i32> {
    let state = reducer::replay_store(store)?;
    if let Some(existing) = existing_idempotent_message(&state, &actor, &draft)? {
        println!("{existing}");
        return Ok(0);
    }

    let msg_id = ids::new_msg_id();
    let correlation_id = if draft.msg_kind == "request" && draft.correlation_id.is_none() {
        Some(msg_id.clone())
    } else {
        draft.correlation_id.clone()
    };
    let op = make_msg_send_with_metadata(
        actor.clone(),
        msg_id.clone(),
        draft.to.clone(),
        draft.entity.clone(),
        draft.reservation.clone(),
        draft.msg_kind.clone(),
        draft.body.clone(),
        draft.reply_to.clone(),
        correlation_id,
        draft.idempotency_key.clone(),
        Timestamp::now(),
    );
    let name = publish::publish_op(store, &op)?;
    let state = reducer::replay_store(store)?;
    if state.was_accepted(name.as_str()) {
        println!("{msg_id}");
        return Ok(0);
    }

    // Two publishers can race after both pass the preflight read. Treat the
    // losing duplicate as the same successful send when its content matches.
    if let Some(existing) = existing_idempotent_message(&state, &actor, &draft)? {
        println!("{existing}");
        return Ok(0);
    }
    let reason = state
        .rejection_reason(name.as_str())
        .unwrap_or_else(|| "unknown".into());
    eprintln!("rejected: {reason}");
    Ok(2)
}

fn cmd_msg(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    json_mode: bool,
    cmd: MsgCmd,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;
    match cmd {
        MsgCmd::Send {
            to,
            issue,
            reservation,
            msg_kind,
            idempotency_key,
            text,
        } => {
            if !validate_msg_kind(&msg_kind) {
                return Err(MoteError::Invalid(format!(
                    "invalid msg_kind `{msg_kind}` (expected one of: {})",
                    op::VALID_MSG_KINDS.join(" | ")
                )));
            }
            if op::VALID_REPLY_KINDS.contains(&msg_kind.as_str()) {
                return Err(MoteError::Invalid(format!(
                    "message kind `{msg_kind}` requires `mote msg reply <request-id>`"
                )));
            }
            publish_message(
                &store,
                actor,
                MessageDraft {
                    to,
                    entity: issue,
                    reservation,
                    msg_kind,
                    body: text,
                    reply_to: None,
                    correlation_id: None,
                    idempotency_key,
                },
            )
        }
        MsgCmd::Ack { msg_id } => {
            let op = make_msg_ack(actor, msg_id, Timestamp::now());
            let name = publish::publish_op(&store, &op)?;
            verify_accept(&store, &name)
        }
        MsgCmd::Reply {
            msg_id,
            msg_kind,
            idempotency_key,
            text,
        } => {
            if !op::VALID_REPLY_KINDS.contains(&msg_kind.as_str()) {
                return Err(MoteError::Invalid(format!(
                    "invalid reply kind `{msg_kind}` (expected one of: {})",
                    op::VALID_REPLY_KINDS.join(" | ")
                )));
            }
            let state = reducer::replay_store(&store)?;
            let request = state
                .messages
                .get(&msg_id)
                .ok_or_else(|| MoteError::Invalid(format!("no such request `{msg_id}`")))?;
            if request.reply_to.is_some() || request.msg_kind != "request" {
                return Err(MoteError::Invalid(format!(
                    "message `{msg_id}` is not a root request"
                )));
            }
            if request.to != actor {
                return Err(MoteError::Invalid(format!(
                    "request `{msg_id}` is addressed to {}, not {actor}",
                    request.to
                )));
            }
            let draft = MessageDraft {
                to: request.from.clone(),
                entity: request.entity.clone(),
                reservation: request.reservation.clone(),
                msg_kind,
                body: text,
                reply_to: Some(msg_id.clone()),
                correlation_id: Some(
                    request
                        .correlation_id
                        .clone()
                        .unwrap_or_else(|| msg_id.clone()),
                ),
                idempotency_key,
            };
            if request.request_state != Some(RequestState::Open) {
                if let Some(existing) = existing_idempotent_message(&state, &actor, &draft)? {
                    println!("{existing}");
                    return Ok(0);
                }
                return Err(MoteError::Invalid(format!(
                    "request `{msg_id}` is not open"
                )));
            }
            publish_message(&store, actor, draft)
        }
        MsgCmd::Requests { request_state } => {
            let requested = match request_state.as_deref() {
                Some(value) => Some(RequestState::parse(value).ok_or_else(|| {
                    MoteError::Invalid(format!(
                        "invalid request state `{value}` (expected open | responded | declined | resolved)"
                    ))
                })?),
                None => None,
            };
            let state = reducer::replay_store(&store)?;
            let requests: Vec<&MsgRecord> = state
                .requests_for(&actor)
                .into_iter()
                .filter(|request| requested.is_none_or(|s| request.request_state == Some(s)))
                .collect();
            if json_mode {
                let rows: Vec<_> = requests
                    .iter()
                    .map(|request| request_json(request))
                    .collect();
                println!("{}", serde_json::to_string(&rows)?);
            } else {
                for request in requests {
                    println!(
                        "{}  {}  from={}  to={}  state={}  {}",
                        request.msg_id,
                        request.sent_ts,
                        request.from,
                        request.to,
                        request
                            .request_state
                            .expect("requests_for returned a non-request")
                            .as_str(),
                        request.body
                    );
                }
            }
            Ok(0)
        }
        MsgCmd::Resolve { msg_id } => {
            let op = make_msg_resolve(actor, msg_id, Timestamp::now());
            let name = publish::publish_op(&store, &op)?;
            verify_accept(&store, &name)
        }
    }
}

fn request_json(request: &MsgRecord) -> serde_json::Value {
    serde_json::json!({
        "msg_id": request.msg_id,
        "from": request.from,
        "to": request.to,
        "entity": request.entity,
        "reservation": request.reservation,
        "msg_kind": request.msg_kind,
        "body": request.body,
        "reply_to": request.reply_to,
        "correlation_id": request.correlation_id,
        "idempotency_key": request.idempotency_key,
        "request_state": request.request_state.map(RequestState::as_str),
        "response_msg_id": request.response_msg_id,
        "resolved_op_id": request.resolved_op_id,
        "resolved_ts": request.resolved_ts,
        "sent_ts": request.sent_ts,
        "ack_ts": request.ack_ts,
    })
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
            body,
            text,
        } => {
            let actor = store.resolve_actor(actor_flag)?;
            let topic = normalize_discussion_topic(&topic)?;
            let text = match (text, body) {
                (Some(_), Some(_)) => {
                    return Err(MoteError::Invalid(
                        "provide discussion post text either as positional text or --body, not both"
                            .into(),
                    ));
                }
                (Some(text), None) | (None, Some(text)) => text,
                (None, None) => {
                    return Err(MoteError::Invalid(
                        "discussion post text is required (positional text or --body)".into(),
                    ));
                }
            };
            if text.trim().is_empty() {
                return Err(MoteError::Invalid(
                    "discussion post text must be non-empty".into(),
                ));
            }
            publish_discussion_post(
                &store, actor, &topic, text, reply_to, None, json_mode, "posted",
            )
        }
        DiscussCmd::List { topic, limit } => {
            let state = reducer::replay_store(&store)?;
            let normalized_topic = topic
                .as_deref()
                .map(normalize_discussion_topic)
                .transpose()?;
            let mut posts = state.board_posts_for(normalized_topic.as_deref());
            if let Some(limit) = limit {
                posts = limit_board_posts_preserving_stickies(posts, limit);
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
                if let Some(topic) = normalized_topic.as_deref() {
                    eprintln!("no posts in topic {topic}");
                } else {
                    eprintln!("no discussion posts");
                }
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
            DiscussTopicCmd::New {
                topic,
                title,
                description,
                body,
            } => {
                let actor = store.resolve_actor(actor_flag)?;
                let topic = normalize_discussion_topic(&topic)?;
                if body.as_deref().is_some_and(|text| text.trim().is_empty()) {
                    return Err(MoteError::Invalid(
                        "initial post body must be non-empty".into(),
                    ));
                }

                let topic_op = make_board_topic(
                    actor.clone(),
                    topic.clone(),
                    title,
                    description,
                    Timestamp::now(),
                );
                let topic_name = publish::publish_op(&store, &topic_op)?;
                let state = reducer::replay_store(&store)?;
                if !state.was_accepted(topic_name.as_str()) {
                    let reason = state
                        .rejection_reason(topic_name.as_str())
                        .unwrap_or_else(|| "unknown".into());
                    eprintln!("rejected: {reason}");
                    return Ok(2);
                }

                let mut initial_post_id = None;
                if let Some(body) = body {
                    let post_id = ids::new_post_id();
                    let post_op = make_board_post(
                        actor,
                        post_id.clone(),
                        topic.clone(),
                        body,
                        None,
                        Timestamp::now(),
                    );
                    let post_name = publish::publish_op(&store, &post_op)?;
                    let state = reducer::replay_store(&store)?;
                    if !state.was_accepted(post_name.as_str()) {
                        let reason = state
                            .rejection_reason(post_name.as_str())
                            .unwrap_or_else(|| "unknown".into());
                        eprintln!("initial post rejected: {reason}");
                        return Ok(2);
                    }
                    initial_post_id = Some(post_id);
                }

                // Report the post count the board will actually show, so a
                // caller never has to guess whether its text became visible.
                let state = reducer::replay_store(&store)?;
                let posts = state
                    .board_topics
                    .get(&topic)
                    .map(|t| t.post_count)
                    .unwrap_or(0);

                if json_mode {
                    let v = serde_json::json!({
                        "topic": topic,
                        "initial_post_id": initial_post_id,
                        "posts": posts,
                        "visible_in_list": posts > 0,
                    });
                    println!("{}", serde_json::to_string(&v)?);
                } else {
                    println!("{topic}");
                    match initial_post_id.as_deref() {
                        Some(post_id) => eprintln!(
                            "created topic {topic} with initial post {post_id} (posts={posts})"
                        ),
                        None => eprintln!(
                            "created topic {topic} with no posts (posts=0); \
                             run `mote discuss post --topic {topic} --body ...` to make it visible"
                        ),
                    }
                }
                Ok(0)
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
        DiscussCmd::Decision { topic, body, text } => {
            let actor = store.resolve_actor(actor_flag)?;
            let topic = normalize_discussion_topic(&topic)?;
            let text = require_post_text(text, body, "decision")?;
            let (code, post_id) = publish_discussion_post_id(
                &store,
                actor.clone(),
                &topic,
                text,
                None,
                Some("decision".to_string()),
                json_mode,
                "recorded decision",
            )?;
            if code == 0 {
                // Decisions are what late arrivals need first, so pin them.
                let op = make_board_sticky(actor, post_id, true, Timestamp::now());
                let name = publish::publish_op(&store, &op)?;
                return verify_accept(&store, &name);
            }
            Ok(code)
        }
        DiscussCmd::Summary { topic, body, text } => {
            let topic = normalize_discussion_topic(&topic)?;
            let Some(text) = text.or(body) else {
                // No text: read the pinned summary rather than write one.
                let state = reducer::replay_store(&store)?;
                return print_topic_summary(&state, &topic, json_mode);
            };
            if text.trim().is_empty() {
                return Err(MoteError::Invalid("summary text must be non-empty".into()));
            }
            let actor = store.resolve_actor(actor_flag)?;
            let (code, post_id) = publish_discussion_post_id(
                &store,
                actor.clone(),
                &topic,
                text,
                None,
                Some("summary".to_string()),
                json_mode,
                "set summary",
            )?;
            if code == 0 {
                let op = make_board_sticky(actor, post_id, true, Timestamp::now());
                let name = publish::publish_op(&store, &op)?;
                return verify_accept(&store, &name);
            }
            Ok(code)
        }
        DiscussCmd::Route {
            post_id,
            topic,
            issue,
            note,
        } => {
            let actor = store.resolve_actor(actor_flag)?;
            let (post_id, topic) = route_target(post_id, topic)?;
            let op = make_board_route(
                actor.clone(),
                post_id.clone(),
                topic.clone(),
                "routed".into(),
                Some(issue.clone()),
                note.clone(),
                Timestamp::now(),
            );
            let name = publish::publish_op(&store, &op)?;
            let code = verify_accept(&store, &name)?;
            if code == 0 {
                // Mirror the link onto the bead so `mote show` carries the
                // provenance without a board lookup.
                let target = post_id
                    .clone()
                    .unwrap_or_else(|| format!("topic {}", topic.clone().unwrap_or_default()));
                let text = match note {
                    Some(note) => format!("routed from discussion {target}: {note}"),
                    None => format!("routed from discussion {target}"),
                };
                let note_op = make_note(
                    actor,
                    issue.clone(),
                    "decision".into(),
                    text,
                    Timestamp::now(),
                );
                let _ = publish::publish_op(&store, &note_op);
                print_route_result(
                    post_id.as_deref(),
                    topic.as_deref(),
                    "routed",
                    Some(&issue),
                    json_mode,
                )?;
            }
            Ok(code)
        }
        DiscussCmd::NeedsBead { post_id, topic } => {
            let actor = store.resolve_actor(actor_flag)?;
            let (post_id, topic) = route_target(post_id, topic)?;
            let op = make_board_route(
                actor,
                post_id.clone(),
                topic.clone(),
                "needs_bead".into(),
                None,
                None,
                Timestamp::now(),
            );
            let name = publish::publish_op(&store, &op)?;
            let code = verify_accept(&store, &name)?;
            if code == 0 {
                print_route_result(
                    post_id.as_deref(),
                    topic.as_deref(),
                    "needs_bead",
                    None,
                    json_mode,
                )?;
            }
            Ok(code)
        }
        DiscussCmd::Resolve { post_id, topic } => {
            let actor = store.resolve_actor(actor_flag)?;
            let (post_id, topic) = route_target(post_id, topic)?;
            let op = make_board_route(
                actor,
                post_id.clone(),
                topic.clone(),
                "resolved".into(),
                None,
                None,
                Timestamp::now(),
            );
            let name = publish::publish_op(&store, &op)?;
            let code = verify_accept(&store, &name)?;
            if code == 0 {
                print_route_result(
                    post_id.as_deref(),
                    topic.as_deref(),
                    "resolved",
                    None,
                    json_mode,
                )?;
            }
            Ok(code)
        }
        DiscussCmd::Unrouted { topic } => {
            let state = reducer::replay_store(&store)?;
            let normalized_topic = topic
                .as_deref()
                .map(normalize_discussion_topic)
                .transpose()?;
            let posts = state.unrouted_posts(normalized_topic.as_deref());
            let topics = state.unrouted_topics(normalized_topic.as_deref());
            if json_mode {
                let v = serde_json::json!({
                    "topics": topics.iter().map(|t| topic_json(t)).collect::<Vec<_>>(),
                    "posts": posts.iter().map(|p| board_post_json(p)).collect::<Vec<_>>(),
                });
                println!("{}", serde_json::to_string(&v)?);
            } else {
                for t in &topics {
                    println!(
                        "topic  {}  needs-bead  last={}  {}",
                        t.topic, t.last_activity_ts, t.title
                    );
                }
                for p in &posts {
                    println!(
                        "post   {}  needs-bead  {}  from={}  topic={}  {}",
                        p.post_id, p.sent_ts, p.from, p.topic, p.body
                    );
                }
            }
            Ok(0)
        }
        DiscussCmd::Promote {
            post_id,
            title,
            body,
            priority,
            tags,
            deps,
        } => cmd_discuss_promote(
            &store, actor_flag, json_mode, post_id, title, body, priority, tags, deps,
        ),
    }
}

/// Shared post publisher: mints the id, publishes, and reports the topic's
/// resulting post count so callers can confirm the text is actually visible.
/// Returns the minted post id alongside the exit code; callers that need to
/// act on the post (e.g. pin it) use that rather than re-deriving it from
/// state, which would be ambiguous under concurrent posting.
#[allow(clippy::too_many_arguments)]
fn publish_discussion_post_id(
    store: &Store,
    actor: String,
    topic: &str,
    text: String,
    reply_to: Option<String>,
    post_kind: Option<String>,
    json_mode: bool,
    verb: &str,
) -> MoteResult<(i32, String)> {
    let post_id = ids::new_post_id();
    let op = op::make_board_post_of_kind(
        actor,
        post_id.clone(),
        topic.to_string(),
        text,
        reply_to.clone(),
        post_kind.clone(),
        Timestamp::now(),
    );
    let name = publish::publish_op(store, &op)?;
    let state = reducer::replay_store(store)?;
    if !state.was_accepted(name.as_str()) {
        let reason = state
            .rejection_reason(name.as_str())
            .unwrap_or_else(|| "unknown".into());
        eprintln!("rejected: {reason}");
        return Ok((2, post_id));
    }
    let posts = state
        .board_topics
        .get(topic)
        .map(|t| t.post_count)
        .unwrap_or(0);

    if json_mode {
        let v = serde_json::json!({
            "post_id": post_id,
            "topic": topic,
            "reply_to": reply_to,
            "post_kind": post_kind.as_deref().unwrap_or("post"),
            "posts": posts,
            "visible_in_list": state.board_posts.contains_key(&post_id),
        });
        println!("{}", serde_json::to_string(&v)?);
    } else {
        println!("{post_id}");
        eprintln!("{verb} {post_id} in topic {topic} (posts={posts})");
    }
    Ok((0, post_id))
}

#[allow(clippy::too_many_arguments)]
fn publish_discussion_post(
    store: &Store,
    actor: String,
    topic: &str,
    text: String,
    reply_to: Option<String>,
    post_kind: Option<String>,
    json_mode: bool,
    verb: &str,
) -> MoteResult<i32> {
    let (code, _) = publish_discussion_post_id(
        store, actor, topic, text, reply_to, post_kind, json_mode, verb,
    )?;
    Ok(code)
}

fn require_post_text(text: Option<String>, body: Option<String>, what: &str) -> MoteResult<String> {
    let text = match (text, body) {
        (Some(_), Some(_)) => {
            return Err(MoteError::Invalid(format!(
                "provide {what} text either as positional text or --body, not both"
            )));
        }
        (Some(text), None) | (None, Some(text)) => text,
        (None, None) => {
            return Err(MoteError::Invalid(format!(
                "{what} text is required (positional text or --body)"
            )));
        }
    };
    if text.trim().is_empty() {
        return Err(MoteError::Invalid(format!("{what} text must be non-empty")));
    }
    Ok(text)
}

/// Normalize a `post_id`-or-`--topic` target pair into exactly one of the two.
fn route_target(
    post_id: Option<String>,
    topic: Option<String>,
) -> MoteResult<(Option<String>, Option<String>)> {
    match (post_id, topic) {
        (Some(post_id), None) => Ok((Some(post_id), None)),
        (None, Some(topic)) => Ok((None, Some(normalize_discussion_topic(&topic)?))),
        (Some(_), Some(_)) => Err(MoteError::Invalid(
            "pass a post id or --topic, not both".into(),
        )),
        (None, None) => Err(MoteError::Invalid(
            "pass a post id or --topic to identify the routing target".into(),
        )),
    }
}

fn print_route_result(
    post_id: Option<&str>,
    topic: Option<&str>,
    route_state: &str,
    issue: Option<&str>,
    json_mode: bool,
) -> MoteResult<()> {
    if json_mode {
        let v = serde_json::json!({
            "post_id": post_id,
            "topic": topic,
            "route_state": route_state,
            "issue": issue,
        });
        println!("{}", serde_json::to_string(&v)?);
    } else {
        let target = match (post_id, topic) {
            (Some(post_id), _) => format!("post {post_id}"),
            (None, Some(topic)) => format!("topic {topic}"),
            (None, None) => "?".to_string(),
        };
        match issue {
            Some(issue) => println!("{target} route_state={route_state} issue={issue}"),
            None => println!("{target} route_state={route_state}"),
        }
    }
    Ok(())
}

fn print_topic_summary(
    state: &crate::state::State,
    topic: &str,
    json_mode: bool,
) -> MoteResult<i32> {
    let Some(record) = state.board_topics.get(topic) else {
        return Err(MoteError::Invalid(format!(
            "no such discussion topic {topic}"
        )));
    };
    let summary = record
        .summary_post_id
        .as_deref()
        .and_then(|id| state.board_posts.get(id));
    if json_mode {
        let v = serde_json::json!({
            "topic": topic,
            "summary_post_id": record.summary_post_id,
            "summary": summary.map(|p| p.body.clone()),
            "decision_count": record.decision_count,
            "route_state": record.route.state.as_str(),
            "issues": record.route.issues.iter().collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string(&v)?);
    } else {
        match summary {
            Some(post) => {
                println!("{}  {}  from={}", post.post_id, post.sent_ts, post.from);
                println!("{}", post.body);
            }
            None => eprintln!(
                "no summary for topic {topic}; \
                 set one with `mote discuss summary --topic {topic} --body ...`"
            ),
        }
    }
    Ok(0)
}

#[allow(clippy::too_many_arguments)]
fn cmd_discuss_promote(
    store: &Store,
    actor_flag: Option<&str>,
    json_mode: bool,
    post_id: String,
    title: Option<String>,
    body: Option<String>,
    priority: Option<i32>,
    tags: Vec<String>,
    deps: Vec<String>,
) -> MoteResult<i32> {
    let actor = store.resolve_actor(actor_flag)?;
    let state = reducer::replay_store(store)?;
    let Some(post) = state.board_posts.get(&post_id) else {
        return Err(MoteError::Invalid(format!("no such post {post_id}")));
    };
    if let Some(p) = priority {
        if !(0..=3).contains(&p) {
            return Err(MoteError::Invalid(format!("priority {p} out of 0..=3")));
        }
    }

    // The post already carries the argument; the bead only needs a handle on it
    // plus a pointer back, so the board never becomes a second task tracker.
    let title = match title {
        Some(title) if !title.trim().is_empty() => title,
        _ => first_line_title(&post.body),
    };
    let provenance = format!(
        "promoted from discussion post {} in topic {}",
        post.post_id, post.topic
    );
    let body = match body {
        Some(body) => format!("{body}\n\n{provenance}"),
        None => format!("{}\n\n{provenance}", post.body),
    };
    let topic = post.topic.clone();

    let bead_id = ids::new_bead_id();
    let set = ScalarSet {
        title: Some(title.clone()),
        status: Some(Status::Open),
        priority,
        body: Some(body),
        ..Default::default()
    };
    let create = make_create(actor.clone(), bead_id.clone(), set, Timestamp::now());
    let create_name = publish::publish_op(store, &create)?;
    let state = reducer::replay_store(store)?;
    if !state.was_accepted(create_name.as_str()) {
        let reason = state
            .rejection_reason(create_name.as_str())
            .unwrap_or_else(|| "unknown".into());
        eprintln!("create rejected: {reason}");
        return Ok(2);
    }

    let mut had_failure = false;
    let mut names = Vec::new();
    for t in &tags {
        let op = make_tag(
            true,
            actor.clone(),
            bead_id.clone(),
            t.clone(),
            Timestamp::now(),
        );
        names.push(publish::publish_op(store, &op)?);
    }
    for d in &deps {
        let op = make_dep(
            true,
            actor.clone(),
            bead_id.clone(),
            d.clone(),
            "blocks".into(),
            Timestamp::now(),
        );
        names.push(publish::publish_op(store, &op)?);
    }

    // Route last: the link is only meaningful once the bead exists.
    let route = make_board_route(
        actor.clone(),
        Some(post_id.clone()),
        None,
        "routed".into(),
        Some(bead_id.clone()),
        Some(format!("promoted to {bead_id}")),
        Timestamp::now(),
    );
    names.push(publish::publish_op(store, &route)?);

    let state = reducer::replay_store(store)?;
    for n in &names {
        if !state.was_accepted(n.as_str()) {
            had_failure = true;
            let reason = state
                .rejection_reason(n.as_str())
                .unwrap_or_else(|| "unknown".into());
            eprintln!("{} rejected: {reason}", n.as_str());
        }
    }

    if json_mode {
        let v = serde_json::json!({
            "id": bead_id,
            "title": title,
            "post_id": post_id,
            "topic": topic,
            "route_state": state
                .board_posts
                .get(&post_id)
                .map(|p| p.route.state.as_str()),
        });
        println!("{}", serde_json::to_string(&v)?);
    } else {
        println!("{bead_id}");
        eprintln!("promoted post {post_id} (topic {topic}) to {bead_id}");
    }
    Ok(if had_failure { 2 } else { 0 })
}

/// First non-empty line of a post body, truncated to a usable bead title.
fn first_line_title(body: &str) -> String {
    let line = body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("untitled");
    if line.chars().count() <= 120 {
        return line.to_string();
    }
    let truncated: String = line.chars().take(117).collect();
    format!("{truncated}...")
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
            let summary = if t.summary_post_id.is_some() {
                "  summary=yes"
            } else {
                ""
            };
            println!(
                "{}  posts={}  sticky={}  decisions={}{}  created={}  last={}  {}{}  {}",
                t.topic,
                t.post_count,
                t.sticky_count,
                t.decision_count,
                summary,
                t.created_ts,
                t.last_activity_ts,
                explicit,
                route_marker(&t.route),
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
        "decision_count": t.decision_count,
        "summary_post_id": t.summary_post_id,
        "route_state": t.route.state.as_str(),
        "issues": t.route.issues.iter().collect::<Vec<_>>(),
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
        posts = limit_board_posts_preserving_stickies(posts, limit);
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
                "{}{}{}  {}  from={}  topic={}  reply={}{}  {}",
                p.post_id,
                sticky,
                post_kind_marker(&p.post_kind),
                p.sent_ts,
                p.from,
                p.topic,
                reply,
                route_marker(&p.route),
                p.body
            );
        }
    }
    Ok(0)
}

/// Non-default post kinds are called out inline; a plain post prints nothing
/// extra so existing output stays unchanged.
fn post_kind_marker(post_kind: &str) -> String {
    if post_kind == "post" {
        String::new()
    } else {
        format!(" {post_kind}")
    }
}

fn route_marker(route: &crate::state::RouteRecord) -> String {
    if route.state == crate::state::RouteState::Open && route.issues.is_empty() {
        return String::new();
    }
    let issues: Vec<&str> = route.issues.iter().map(String::as_str).collect();
    if issues.is_empty() {
        format!("  route={}", route.state.as_str())
    } else {
        format!(
            "  route={} issues={}",
            route.state.as_str(),
            issues.join(",")
        )
    }
}

fn limit_board_posts_preserving_stickies(
    posts: Vec<&crate::state::BoardPostRecord>,
    limit: usize,
) -> Vec<&crate::state::BoardPostRecord> {
    if limit == 0 {
        return Vec::new();
    }
    if posts.len() <= limit {
        return posts;
    }
    let mut sticky = Vec::new();
    let mut non_sticky = Vec::new();
    for post in posts {
        if post.sticky {
            sticky.push(post);
        } else {
            non_sticky.push(post);
        }
    }
    if sticky.len() >= limit {
        sticky.truncate(limit);
        return sticky;
    }
    let keep_non_sticky = limit - sticky.len();
    if non_sticky.len() > keep_non_sticky {
        non_sticky = non_sticky.split_off(non_sticky.len() - keep_non_sticky);
    }
    sticky.extend(non_sticky);
    sticky
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
        "post_kind": p.post_kind,
        "sticky": p.sticky,
        "sticky_op_id": p.sticky_op_id,
        "route_state": p.route.state.as_str(),
        "issues": p.route.issues.iter().collect::<Vec<_>>(),
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

#[allow(clippy::too_many_arguments)]
fn cmd_inbox(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    json_mode: bool,
    issue: Option<String>,
    from: Option<String>,
    kind: Option<String>,
    follow: bool,
    wait: bool,
    timeout: Option<u64>,
    after: Option<String>,
    interval: u64,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;

    if follow {
        return cmd_inbox_follow(
            &store,
            &actor,
            json_mode,
            issue.as_deref(),
            from.as_deref(),
            kind.as_deref(),
            after.as_deref(),
            interval,
        );
    }
    if after.is_some() {
        return Err(MoteError::Invalid("inbox --after requires --follow".into()));
    }
    if wait {
        return cmd_inbox_wait(
            &store,
            &actor,
            json_mode,
            issue.as_deref(),
            from.as_deref(),
            kind.as_deref(),
            Duration::from_secs(timeout.unwrap_or(60)),
            interval,
        );
    }

    let state = reducer::replay_store(&store)?;
    let filtered = filtered_inbox(
        &state,
        &actor,
        issue.as_deref(),
        from.as_deref(),
        kind.as_deref(),
    );
    write_inbox_messages(&filtered, json_mode)?;
    Ok(0)
}

fn filtered_inbox<'a>(
    state: &'a crate::state::State,
    actor: &str,
    issue: Option<&str>,
    from: Option<&str>,
    kind: Option<&str>,
) -> Vec<&'a MsgRecord> {
    state
        .inbox_for(actor)
        .into_iter()
        .filter(|m| {
            issue.is_none_or(|i| m.entity.as_deref() == Some(i))
                && from.is_none_or(|f| m.from == f)
                && kind.is_none_or(|k| m.msg_kind == k)
        })
        .collect()
}

fn write_inbox_messages(messages: &[&MsgRecord], json_mode: bool) -> MoteResult<()> {
    if json_mode {
        let arr: Vec<_> = messages
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
                    "reply_to": m.reply_to,
                    "correlation_id": m.correlation_id,
                    "idempotency_key": m.idempotency_key,
                    "request_state": m.request_state.map(RequestState::as_str),
                    "response_msg_id": m.response_msg_id,
                    "sent_ts": m.sent_ts,
                })
            })
            .collect();
        println!("{}", serde_json::to_string(&arr)?);
    } else {
        for m in messages {
            let issue_s = m.entity.as_deref().unwrap_or("-");
            println!(
                "{}  {}  from={}  issue={}  kind={}  {}",
                m.msg_id, m.sent_ts, m.from, issue_s, m.msg_kind, m.body
            );
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_inbox_wait(
    store: &Store,
    actor: &str,
    json_mode: bool,
    issue: Option<&str>,
    from: Option<&str>,
    kind: Option<&str>,
    timeout: Duration,
    interval: u64,
) -> MoteResult<i32> {
    let filter = crate::events::EventFilter::messages_for(actor);
    let mut tailer = crate::events::EventTailer::new(store, None, interval)?;
    let baseline = crate::events::state_for_names(store, tailer.initial_names())?;
    let pending = filtered_inbox(&baseline, actor, issue, from, kind);
    if !pending.is_empty() {
        write_inbox_messages(&pending, json_mode)?;
        return Ok(0);
    }
    if timeout.is_zero() {
        write_inbox_messages(&[], json_mode)?;
        return Ok(0);
    }

    tailer.start(store)?;
    if tailer
        .poll(store, &filter)?
        .iter()
        .any(|event| inbox_event_matches(event, actor, issue, from, kind))
    {
        let state = reducer::replay_store(store)?;
        let messages = filtered_inbox(&state, actor, issue, from, kind);
        write_inbox_messages(&messages, json_mode)?;
        return Ok(0);
    }

    let started = Instant::now();
    let fallback = Duration::from_secs(interval.max(1));
    while let Some(remaining) = timeout.checked_sub(started.elapsed()) {
        if remaining.is_zero() {
            break;
        }
        // Scan at the fallback cadence even if the platform watcher is quiet.
        // The final iteration uses the shorter remaining deadline.
        let _ = tailer.wait_timeout(remaining.min(fallback));
        if tailer
            .poll(store, &filter)?
            .iter()
            .any(|event| inbox_event_matches(event, actor, issue, from, kind))
        {
            let state = reducer::replay_store(store)?;
            let messages = filtered_inbox(&state, actor, issue, from, kind);
            write_inbox_messages(&messages, json_mode)?;
            return Ok(0);
        }
    }

    // The filesystem watcher is a latency optimization, not the source of
    // truth. Replay once at the deadline so a missed/coalesced notification or
    // a fallback tick racing the timeout cannot hide a durable delivery.
    let state = reducer::replay_store(store)?;
    let messages = filtered_inbox(&state, actor, issue, from, kind);
    write_inbox_messages(&messages, json_mode)?;
    Ok(0)
}

#[allow(clippy::too_many_arguments)]
fn cmd_inbox_follow(
    store: &Store,
    actor: &str,
    json_mode: bool,
    issue: Option<&str>,
    from: Option<&str>,
    kind: Option<&str>,
    after: Option<&str>,
    interval: u64,
) -> MoteResult<i32> {
    let filter = crate::events::EventFilter::messages_for(actor);
    let mut tailer = crate::events::EventTailer::new(store, after, interval)?;

    // Without a cursor, begin with the inbox as it existed at the tailer's
    // baseline. Replaying exactly those names avoids duplicating a message that
    // arrives while the follow stream is being established.
    if after.is_none() {
        let baseline = crate::events::state_for_names(store, tailer.initial_names())?;
        let unacked: BTreeSet<String> = baseline
            .inbox_for(actor)
            .into_iter()
            .filter(|m| {
                issue.is_none_or(|i| m.entity.as_deref() == Some(i))
                    && from.is_none_or(|f| m.from == f)
                    && kind.is_none_or(|k| m.msg_kind == k)
            })
            .map(|m| m.msg_id.clone())
            .collect();
        let initial =
            crate::events::accepted_events_for_names(store, tailer.initial_names(), &filter)?;
        for event in initial
            .iter()
            .filter(|event| inbox_event_matches(event, actor, issue, from, kind))
            .filter(|event| {
                event.data["msg_id"]
                    .as_str()
                    .is_some_and(|msg_id| unacked.contains(msg_id))
            })
        {
            crate::events::write_inbox_event(event, json_mode)?;
        }
    }

    // Emit cursor catch-up before notification setup, then install the watcher
    // and scan once more to close that installation gap.
    for event in tailer
        .poll(store, &filter)?
        .iter()
        .filter(|event| inbox_event_matches(event, actor, issue, from, kind))
    {
        crate::events::write_inbox_event(event, json_mode)?;
    }
    tailer.start(store)?;
    loop {
        for event in tailer
            .poll(store, &filter)?
            .iter()
            .filter(|event| inbox_event_matches(event, actor, issue, from, kind))
        {
            crate::events::write_inbox_event(event, json_mode)?;
        }
        if !tailer.wait() {
            break;
        }
    }
    Ok(0)
}

fn inbox_event_matches(
    event: &crate::events::EventEnvelope,
    actor: &str,
    issue: Option<&str>,
    from: Option<&str>,
    kind: Option<&str>,
) -> bool {
    matches!(
        event.event_type.as_str(),
        "message.sent" | "message.responded" | "message.declined"
    ) && event.data["to"].as_str() == Some(actor)
        && issue.is_none_or(|i| event.data["entity"].as_str() == Some(i))
        && from.is_none_or(|f| event.actor == f)
        && kind.is_none_or(|k| event.data["msg_kind"].as_str() == Some(k))
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

#[allow(clippy::too_many_arguments)]
fn cmd_begin(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    id: String,
    paths: Vec<String>,
    note: Option<String>,
    ttl: Option<u32>,
    announce: Option<String>,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag)?;
    let format = store.read_format()?;
    let reserve_ttl = ttl.unwrap_or(format.default_ttl_s.reservation);
    let claim_ttl = format.default_ttl_s.claim;

    if paths.is_empty() {
        return Err(MoteError::Invalid("at least one path required".into()));
    }
    // Validate the topic before anything is published, so a typo cannot leave
    // a reservation open with no matching board claim.
    let announce = announce
        .as_deref()
        .map(normalize_discussion_topic)
        .transpose()?;

    // Step 1: reserve_open
    let rv_id = ids::new_reservation_id();
    let paths_for_announce = paths.join(", ");
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

    // Step 3: move open work out of the ready queue after the claim lands.
    if let Some(bead) = state2.beads.get(&id) {
        if bead.status == Status::Open {
            let set = ScalarSet {
                status: Some(Status::Doing),
                ..ScalarSet::default()
            };
            let mut expect = BTreeMap::new();
            expect.insert("status".to_string(), clock_for(bead, "status")?);
            let status_op = make_patch(actor.clone(), id.clone(), expect, set, Timestamp::now());
            let status_name = publish::publish_op(&store, &status_op)?;
            let state3 = reducer::replay_store(&store)?;
            if !state3.was_accepted(status_name.as_str()) {
                let reason = state3
                    .rejection_reason(status_name.as_str())
                    .unwrap_or_else(|| "unknown".into());
                eprintln!("status update rejected: {reason}");
                let close = make_reserve_close(actor.clone(), rv_id, None, Timestamp::now());
                let _ = publish::publish_op(&store, &close);
                let release = make_release(actor, id, None, Timestamp::now());
                let _ = publish::publish_op(&store, &release);
                return Ok(2);
            }
        }
    }

    // Step 4: optional progress note (best effort).
    if let Some(text) = note {
        let note_op = make_note(
            actor.clone(),
            id.clone(),
            "progress".into(),
            text,
            Timestamp::now(),
        );
        let _ = publish::publish_op(&store, &note_op);
    }

    // Step 5: optional claim announcement on the source topic, so board readers
    // see the claim at the same moment `mote ready` stops offering the work.
    if let Some(topic) = announce {
        let post_id = ids::new_post_id();
        let body = format!("claiming {id} for {rv_id} on {}", paths_for_announce);
        let post_op = make_board_post(
            actor,
            post_id.clone(),
            topic.clone(),
            body,
            None,
            Timestamp::now(),
        );
        let post_name = publish::publish_op(&store, &post_op)?;
        let state = reducer::replay_store(&store)?;
        if state.was_accepted(post_name.as_str()) {
            eprintln!("announced {post_id} in topic {topic}");
        } else {
            let reason = state
                .rejection_reason(post_name.as_str())
                .unwrap_or_else(|| "unknown".into());
            // The claim itself succeeded; a failed announcement is reported but
            // does not roll back reserved and claimed work.
            eprintln!("announce rejected: {reason}");
        }
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

/// Session id for this invocation, from `MOTE_SESSION`.
fn env_session_id() -> Option<String> {
    std::env::var("MOTE_SESSION")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn cmd_session(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    json_mode: bool,
    cmd: SessionCmd,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;

    match cmd {
        SessionCmd::Start {
            as_actor,
            ttl,
            label,
        } => {
            // `--as` is the identity for this session; without it we fall back
            // to normal resolution so `session start` still works in a repo
            // that has only ever had one actor.
            let actor = match as_actor.as_deref() {
                Some(name) if !name.trim().is_empty() => name.trim().to_string(),
                Some(_) => return Err(MoteError::Invalid("--as must be non-empty".into())),
                None => store.resolve_actor(actor_flag)?,
            };
            if ttl == 0 {
                return Err(MoteError::Invalid("--ttl must be > 0".into()));
            }
            let session_id = ids::new_session_id();
            let op = make_session_start(
                actor.clone(),
                session_id.clone(),
                ttl,
                label.clone(),
                Some(std::process::id()),
                Timestamp::now(),
            );
            let name = publish::publish_op(&store, &op)?;
            let state = reducer::replay_store(&store)?;
            if !state.was_accepted(name.as_str()) {
                let reason = state
                    .rejection_reason(name.as_str())
                    .unwrap_or_else(|| "unknown".into());
                eprintln!("rejected: {reason}");
                return Ok(2);
            }
            let lease_until = state
                .sessions
                .get(&session_id)
                .map(|s| s.lease_until_ts.clone())
                .unwrap_or_default();

            if json_mode {
                let v = serde_json::json!({
                    "session_id": session_id,
                    "actor": actor,
                    "ttl_s": ttl,
                    "label": label,
                    "lease_until_ts": lease_until,
                    "activate": format!("export MOTE_ACTOR={actor}; export MOTE_SESSION={session_id}"),
                });
                println!("{}", serde_json::to_string(&v)?);
            } else {
                // stdout is shell-evalable on purpose: a CLI cannot set its
                // parent shell's environment, so the caller must apply it.
                println!("export MOTE_ACTOR={actor}");
                println!("export MOTE_SESSION={session_id}");
                eprintln!("session {session_id} for {actor} until {lease_until}");
                eprintln!("activate with: eval \"$(mote session start --as {actor})\"");
            }
            Ok(0)
        }
        SessionCmd::Renew { id, ttl } => {
            let Some(session_id) = id.or_else(env_session_id) else {
                return Err(MoteError::Invalid(
                    "no session id (pass --id or set MOTE_SESSION)".into(),
                ));
            };
            let state = reducer::replay_store(&store)?;
            let Some(session) = state.sessions.get(&session_id) else {
                return Err(MoteError::Invalid(format!("no such session {session_id}")));
            };
            let ttl = ttl.unwrap_or(session.ttl_s);
            let op = make_session_start(
                session.actor.clone(),
                session_id.clone(),
                ttl,
                session.label.clone(),
                Some(std::process::id()),
                Timestamp::now(),
            );
            let name = publish::publish_op(&store, &op)?;
            let code = verify_accept(&store, &name)?;
            if code == 0 {
                let state = reducer::replay_store(&store)?;
                let lease_until = state
                    .sessions
                    .get(&session_id)
                    .map(|s| s.lease_until_ts.clone())
                    .unwrap_or_default();
                if json_mode {
                    let v = serde_json::json!({
                        "session_id": session_id,
                        "ttl_s": ttl,
                        "lease_until_ts": lease_until,
                    });
                    println!("{}", serde_json::to_string(&v)?);
                } else {
                    println!("{session_id}");
                    eprintln!("renewed until {lease_until}");
                }
            }
            Ok(code)
        }
        SessionCmd::List { all } => {
            let state = reducer::replay_store(&store)?;
            let now = ids::format_rfc3339(Timestamp::now());
            let sessions: Vec<&crate::state::SessionRecord> = if all {
                state.sessions.values().collect()
            } else {
                state.live_sessions(&now)
            };
            if json_mode {
                let arr: Vec<_> = sessions.iter().map(|s| session_json(s, &now)).collect();
                println!("{}", serde_json::to_string(&arr)?);
            } else {
                for s in &sessions {
                    let live = if s.is_live(&now) { "live" } else { "ended" };
                    let pid = s.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into());
                    let label = s.label.as_deref().unwrap_or("");
                    println!(
                        "{}  {}  {live}  pid={pid}  until={}  {label}",
                        s.session_id, s.actor, s.lease_until_ts
                    );
                }
            }
            Ok(0)
        }
        SessionCmd::End { id } => {
            let Some(session_id) = id.or_else(env_session_id) else {
                return Err(MoteError::Invalid(
                    "no session id (pass one or set MOTE_SESSION)".into(),
                ));
            };
            let state = reducer::replay_store(&store)?;
            // End under the session's own actor so a session can be closed out
            // from a shell whose MOTE_ACTOR has already been unset.
            let actor = match state.sessions.get(&session_id) {
                Some(session) => session.actor.clone(),
                None => return Err(MoteError::Invalid(format!("no such session {session_id}"))),
            };
            let op = make_session_end(actor, session_id.clone(), Timestamp::now());
            let name = publish::publish_op(&store, &op)?;
            let code = verify_accept(&store, &name)?;
            if code == 0 {
                println!("{session_id}");
            }
            Ok(code)
        }
    }
}

fn session_json(s: &crate::state::SessionRecord, now_ts: &str) -> serde_json::Value {
    serde_json::json!({
        "session_id": s.session_id,
        "actor": s.actor,
        "label": s.label,
        "pid": s.pid,
        "ttl_s": s.ttl_s,
        "started_ts": s.started_ts,
        "lease_until_ts": s.lease_until_ts,
        "ended_ts": s.ended_ts,
        "live": s.is_live(now_ts),
    })
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

/// One-shot "what is being touched right now?" view.
///
/// Replay-only by construction: sessions, reservations, claims, `doing` work
/// and recent topics all come from the op log. The commit list is advisory
/// context read from Git and is labelled as such, because it is the one thing
/// a replay cannot know.
fn cmd_in_flight(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    json_mode: bool,
    minutes: u64,
    include_git: bool,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag).ok();
    let state = reducer::replay_store(&store)?;
    let now = Timestamp::now();
    let now_ts = ids::format_rfc3339(now);
    let cutoff = now
        .checked_sub(jiff::SignedDuration::from_secs(minutes as i64 * 60))
        .map(ids::format_rfc3339)
        .unwrap_or_default();

    let sessions = state.live_sessions(&now_ts);
    let reservations: Vec<_> = state
        .reservations
        .values()
        .filter(|r| r.is_active(&now_ts))
        .collect();
    let doing: Vec<&Bead> = state
        .live_beads()
        .filter(|b| b.status == Status::Doing)
        .collect();
    let claims: Vec<&Bead> = state
        .live_beads()
        .filter(|b| b.claim.as_ref().is_some_and(|c| c.is_live(&now_ts)))
        .collect();
    let topics: Vec<_> = state
        .board_topics_by_activity()
        .into_iter()
        .filter(|t| t.last_activity_ts.as_str() >= cutoff.as_str())
        .collect();
    let commits = if include_git {
        recent_commits(store.root(), minutes)
    } else {
        Vec::new()
    };

    if json_mode {
        let v = serde_json::json!({
            "actor": actor,
            "now_ts": now_ts,
            "window_minutes": minutes,
            "sessions": sessions.iter().map(|s| session_json(s, &now_ts)).collect::<Vec<_>>(),
            "reservations": reservations.iter().map(|r| serde_json::json!({
                "reservation_id": r.reservation_id, "actor": r.actor, "entity": r.entity,
                "paths": r.live_paths(), "lease_until_ts": r.lease_until_ts,
            })).collect::<Vec<_>>(),
            "doing": doing.iter().map(|b| serde_json::json!({
                "id": b.id, "title": b.title, "priority": b.priority,
                "claimed_by": b.claim.as_ref().filter(|c| c.is_live(&now_ts)).map(|c| &c.claimed_by),
                "lease_until_ts": b.claim.as_ref().filter(|c| c.is_live(&now_ts)).map(|c| &c.lease_until_ts),
            })).collect::<Vec<_>>(),
            "claims": claims.iter().map(|b| serde_json::json!({
                "id": b.id, "status": b.status.as_str(),
                "claimed_by": b.claim.as_ref().map(|c| &c.claimed_by),
                "lease_until_ts": b.claim.as_ref().map(|c| &c.lease_until_ts),
            })).collect::<Vec<_>>(),
            "topics": topics.iter().map(|t| {
                let mut v = topic_json(t);
                if let Some(obj) = v.as_object_mut() {
                    let unread = actor.as_deref().map(|a| {
                        state.unread_board_posts_for(a, Some(&t.topic)).len()
                    });
                    obj.insert("unread".into(), serde_json::json!(unread));
                }
                v
            }).collect::<Vec<_>>(),
            "recent_commits_advisory": commits.iter().map(|(sha, subject)| serde_json::json!({
                "sha": sha, "subject": subject,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string(&v)?);
        return Ok(0);
    }

    if let Some(actor) = &actor {
        println!("actor: {actor}  (window: last {minutes}m)");
    } else {
        println!("actor: unresolved  (window: last {minutes}m)");
    }

    println!("\nSESSIONS ({}):", sessions.len());
    for s in &sessions {
        let pid = s.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into());
        let label = s.label.as_deref().unwrap_or("");
        println!(
            "  {}  {}  pid={pid}  until={}  {label}",
            s.actor, s.session_id, s.lease_until_ts
        );
    }
    if sessions.is_empty() {
        println!(
            "  (none — concurrent sessions are indistinguishable without `mote session start`)"
        );
    }

    println!("\nRESERVATIONS ({}):", reservations.len());
    for r in &reservations {
        println!(
            "  {}  {}  by {}  {}  until {}",
            r.live_paths().join(", "),
            r.reservation_id,
            r.actor,
            r.entity,
            r.lease_until_ts
        );
    }

    println!("\nDOING ({}):", doing.len());
    for b in &doing {
        let holder = b
            .claim
            .as_ref()
            .filter(|c| c.is_live(&now_ts))
            .map(|c| c.claimed_by.as_str())
            .unwrap_or("unclaimed");
        println!("  {}  p{}  by {holder}  {}", b.id, b.priority, b.title);
    }

    println!("\nACTIVE TOPICS ({}):", topics.len());
    for t in &topics {
        let unread = actor
            .as_deref()
            .map(|a| state.unread_board_posts_for(a, Some(&t.topic)).len())
            .unwrap_or(0);
        println!(
            "  {}  posts={}  unread={unread}  route={}  last={}",
            t.topic,
            t.post_count,
            t.route.state.as_str(),
            t.last_activity_ts
        );
    }

    if include_git {
        println!(
            "\nRECENT COMMITS ({}) [advisory: read from git, not from replayed state]:",
            commits.len()
        );
        for (sha, subject) in &commits {
            println!("  {sha}  {subject}");
        }
    }
    Ok(0)
}

/// Advisory only. Returns an empty list when Git is unavailable or the store is
/// not inside a work tree; in-flight must never fail because of Git.
fn recent_commits(store_root: &Path, minutes: u64) -> Vec<(String, String)> {
    let repo_dir = store_root.parent().unwrap_or(store_root);
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .arg("log")
        .arg(format!("--since={minutes}.minutes.ago"))
        .arg("--pretty=format:%h %s")
        .arg("--max-count=20")
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (sha, subject) = line.split_once(' ')?;
            Some((sha.to_string(), subject.to_string()))
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn cmd_events(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    json_mode: bool,
    kinds: Vec<String>,
    for_actor: Option<String>,
    after: Option<String>,
    follow: bool,
    interval: u64,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    // An explicit global --actor is a convenient shorthand for --for-actor on
    // this read-only command. Persisted/env actor identity does not silently
    // filter oversight output.
    let actor_filter = for_actor.or_else(|| actor_flag.map(str::to_string));
    let filter = crate::events::EventFilter::new(&kinds, actor_filter)?;

    if follow {
        let mut tailer = crate::events::EventTailer::new(&store, after.as_deref(), interval)?;
        for event in tailer.poll(&store, &filter)? {
            crate::events::write_event(&event, json_mode)?;
        }
        tailer.start(&store)?;
        loop {
            for event in tailer.poll(&store, &filter)? {
                crate::events::write_event(&event, json_mode)?;
            }
            if !tailer.wait() {
                break;
            }
        }
    } else {
        for event in crate::events::accepted_events(&store, after.as_deref(), &filter)? {
            crate::events::write_event(&event, json_mode)?;
        }
    }
    Ok(0)
}

fn cmd_watch(
    actor_flag: Option<&str>,
    store_flag: Option<&Path>,
    json_mode: bool,
    interval: u64,
) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag).ok();
    crate::watch::run(&store, actor.as_deref(), json_mode, interval)
}

fn cmd_ui(actor_flag: Option<&str>, store_flag: Option<&Path>) -> MoteResult<i32> {
    let store = open_store(store_flag)?;
    let actor = store.resolve_actor(actor_flag).ok();
    crate::tui::run(&store, actor.as_deref())
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

/// Actor names that read as "nobody set this", which make every coordination
/// primitive ambiguous the moment a second agent picks the same default.
const SENTINEL_ACTORS: &[&str] = &[
    "agent",
    "assistant",
    "bot",
    "claude",
    "codex",
    "default",
    "me",
    "mote",
    "unknown",
    "unset",
    "user",
];

/// Detect the identity collapse that makes reservations, claims, and co-sign
/// rules meaningless: several concurrent sessions publishing under one name.
///
/// Each mote invocation is its own process, so "this actor published from more
/// than one pid" is true of any ordinary sequence of commands and proves
/// nothing. These checks use evidence that actually discriminates: overlapping
/// live reservations held by one actor (impossible for a single session doing
/// one thing at a time), concurrent session leases, and identity that comes
/// from the checkout-wide file rather than the process.
fn identity_warnings(store: &Store, actor: &ActorResolution) -> MoteResult<Vec<String>> {
    let mut warnings = Vec::new();

    if SENTINEL_ACTORS.contains(&actor.actor.to_ascii_lowercase().as_str()) {
        warnings.push(format!(
            "actor `{}` is a generic default; give each session its own name \
             (`mote session start --as <name>`) so reservations and claims stay attributable",
            actor.actor
        ));
    }

    let state = reducer::replay_store(store)?;
    let now_ts = ids::format_rfc3339(Timestamp::now());

    // Same-actor reservations never conflict by design, so an overlap here is
    // silent: two sessions are editing the same paths believing they are alone.
    for (rv_a, rv_b, path) in overlapping_same_actor_reservations(&state, &actor.actor, &now_ts) {
        warnings.push(format!(
            "reservations {rv_a} and {rv_b} both hold `{path}` under actor `{}`; \
             same-actor reservations do not conflict, so this overlap is invisible to \
             `mote preflight` and `mote who-has`",
            actor.actor
        ));
    }

    let live_sessions = state.live_sessions(&now_ts);
    let own_sessions = state.live_sessions_for(&actor.actor, &now_ts);
    if own_sessions.len() > 1 {
        warnings.push(format!(
            "{} live session leases share actor `{}`; pass distinct `--as` names so board posts \
             and reservations carry different bylines",
            own_sessions.len(),
            actor.actor
        ));
    }
    if actor.source == "local" && live_sessions.len() > 1 {
        warnings.push(format!(
            "actor resolved from `.mote/local/actor` while {} sessions are live; \
             that file is shared by every process in this checkout — prefer MOTE_ACTOR",
            live_sessions.len()
        ));
    }

    Ok(warnings)
}

/// Pairs of live reservations held by one actor that cover a common path.
fn overlapping_same_actor_reservations(
    state: &crate::state::State,
    actor: &str,
    now_ts: &str,
) -> Vec<(String, String, String)> {
    let live: Vec<&crate::state::ReservationState> = state
        .reservations
        .values()
        .filter(|r| r.actor == actor && r.is_active(now_ts))
        .collect();
    let mut out = Vec::new();
    for (i, a) in live.iter().enumerate() {
        for b in live.iter().skip(i + 1) {
            for pa in a.live_paths() {
                for pb in b.live_paths() {
                    if crate::paths::overlap(pa, pb) {
                        out.push((
                            a.reservation_id.clone(),
                            b.reservation_id.clone(),
                            pa.to_string(),
                        ));
                    }
                }
            }
        }
    }
    out
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

    let identity_warnings = match (&actor, storage_ok) {
        (Some(actor), true) => identity_warnings(&store, actor)?,
        _ => Vec::new(),
    };
    // Warnings describe a coordination hazard, not a broken store, so they do
    // not change the exit code — a shared identity still works, it is just
    // ambiguous.
    let ok = storage_ok && actor_ok;

    if json_mode {
        let v = serde_json::json!({
            "ok": ok,
            "warnings": identity_warnings,
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
        if identity_warnings.is_empty() {
            println!("warn:   none");
        } else {
            println!("warn:   {} identity warning(s)", identity_warnings.len());
            for warning in &identity_warnings {
                println!("  - {warning}");
            }
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

struct BundledSkill {
    name: &'static str,
    description: &'static str,
    files: &'static [(&'static str, &'static str)],
}

const BUNDLED_SKILLS: &[BundledSkill] = &[
    BundledSkill {
        name: "mote-tracker",
        description: "Local daemonless issue tracker, claims, path reservations, notes, and handoffs.",
        files: &[
            ("SKILL.md", include_str!("../skills/mote-tracker/SKILL.md")),
            (
                "agents/openai.yaml",
                include_str!("../skills/mote-tracker/agents/openai.yaml"),
            ),
        ],
    },
    BundledSkill {
        name: "mote-message-board",
        description: "Forum-style cross-agent discussion: topics, posts, replies, threads, sticky, unread.",
        files: &[
            (
                "SKILL.md",
                include_str!("../skills/mote-message-board/SKILL.md"),
            ),
            (
                "agents/openai.yaml",
                include_str!("../skills/mote-message-board/agents/openai.yaml"),
            ),
        ],
    },
];

const AGENT_DIRS: &[(&str, &str)] = &[("claude", ".claude/skills"), ("codex", ".codex/skills")];

fn cmd_skills(json_mode: bool, quiet: bool, cmd: SkillsCmd) -> MoteResult<i32> {
    match cmd {
        SkillsCmd::List => cmd_skills_list(json_mode),
        SkillsCmd::Install {
            user,
            repo,
            agents,
            force,
        } => cmd_skills_install(user, repo, agents, force, json_mode, quiet),
    }
}

fn cmd_skills_list(json_mode: bool) -> MoteResult<i32> {
    if json_mode {
        let arr: Vec<_> = BUNDLED_SKILLS
            .iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "description": s.description,
                    "files": s.files.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
                })
            })
            .collect();
        println!("{}", serde_json::to_string(&arr)?);
    } else {
        for skill in BUNDLED_SKILLS {
            println!("{}  {}", skill.name, skill.description);
        }
    }
    Ok(0)
}

fn cmd_skills_install(
    user: bool,
    repo: Option<PathBuf>,
    agents: Vec<String>,
    force: bool,
    json_mode: bool,
    quiet: bool,
) -> MoteResult<i32> {
    if !user && repo.is_none() {
        return Err(MoteError::Invalid(
            "specify --user (install for the current user) or --repo <path> (install into a repo)"
                .into(),
        ));
    }

    let known: Vec<&str> = AGENT_DIRS.iter().map(|(n, _)| *n).collect();
    let selected: Vec<String> = if agents.is_empty() {
        known.iter().map(|s| (*s).to_string()).collect()
    } else {
        for a in &agents {
            if !known.contains(&a.as_str()) {
                return Err(MoteError::Invalid(format!(
                    "unknown agent '{}' (known: {})",
                    a,
                    known.join(",")
                )));
            }
        }
        agents
    };

    let home = if user {
        let h =
            std::env::var_os("HOME").ok_or_else(|| MoteError::Invalid("HOME is not set".into()))?;
        Some(PathBuf::from(h))
    } else {
        None
    };

    let mut installed: Vec<(String, String, PathBuf)> = Vec::new();
    let mut skipped: Vec<(String, String, PathBuf)> = Vec::new();

    for (agent_name, repo_subpath) in AGENT_DIRS {
        if !selected.iter().any(|a| a == agent_name) {
            continue;
        }
        let target_base: PathBuf = if let Some(ref h) = home {
            h.join(format!(".{agent_name}")).join("skills")
        } else {
            repo.as_ref().unwrap().join(repo_subpath)
        };

        for skill in BUNDLED_SKILLS {
            let dest = target_base.join(skill.name);
            let dest_meta = fs::symlink_metadata(&dest).ok();
            if dest_meta.is_some() && !force {
                skipped.push((
                    (*agent_name).to_string(),
                    skill.name.to_string(),
                    dest.clone(),
                ));
                continue;
            }
            if let Some(meta) = dest_meta {
                if meta.file_type().is_symlink() || meta.file_type().is_file() {
                    fs::remove_file(&dest)?;
                } else {
                    fs::remove_dir_all(&dest)?;
                }
            }
            fs::create_dir_all(&dest)?;
            for (rel, content) in skill.files {
                let file_path = dest.join(rel);
                if let Some(parent) = file_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&file_path, content)?;
            }
            installed.push(((*agent_name).to_string(), skill.name.to_string(), dest));
        }
    }

    if json_mode {
        let installed_json: Vec<_> = installed
            .iter()
            .map(|(a, s, p)| {
                serde_json::json!({
                    "agent": a,
                    "skill": s,
                    "path": p.display().to_string(),
                    "status": "installed",
                })
            })
            .collect();
        let skipped_json: Vec<_> = skipped
            .iter()
            .map(|(a, s, p)| {
                serde_json::json!({
                    "agent": a,
                    "skill": s,
                    "path": p.display().to_string(),
                    "status": "skipped",
                })
            })
            .collect();
        let v = serde_json::json!({
            "installed": installed_json,
            "skipped": skipped_json,
        });
        println!("{}", serde_json::to_string(&v)?);
    } else {
        for (_a, _s, p) in &installed {
            println!("installed: {}", p.display());
        }
        if !quiet {
            for (_a, _s, p) in &skipped {
                eprintln!(
                    "skipped (exists; rerun with --force to overwrite): {}",
                    p.display()
                );
            }
        }
    }

    Ok(0)
}

fn open_store(override_path: Option<&Path>) -> MoteResult<Store> {
    let env_path = std::env::var_os("MOTE_STORE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if let Some(p) = override_path.or(env_path.as_deref()) {
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
