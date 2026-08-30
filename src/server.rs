//! Small blocking HTTP/1.1 core for the loopback console server.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Mutex, RwLock, Weak};
use std::time::Duration;

use rand::RngCore;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::errors::{MoteError, MoteResult};
use crate::events::{EventEnvelope, EventFilter, EventTailer};
use crate::ids;
use crate::op::{self, Op, ScalarSet, Status};
use crate::publish;
use crate::reducer;
use crate::repo::Store;
use crate::state::{MsgRecord, State};

const MAX_REQUEST_LINE: usize = 8 * 1024;
const MAX_HEADERS: usize = 32 * 1024;
const MAX_BODY: usize = 1024 * 1024;
const MAX_CONNECTIONS: usize = 64;
const TOKEN_FILE: &str = "serve-token";
const CONSOLE_COOKIE: &str = "mote_console_token";
const CONSOLE_INDEX: &[u8] = include_bytes!("../web/dist/index.html");
const CONSOLE_SCRIPT: &[u8] = include_bytes!("../web/dist/console.js");
const CONSOLE_STYLE: &[u8] = include_bytes!("../web/dist/console.css");

/// Per-launch request boundary for the local console.
///
/// This is public so socket-level integration tests and embedders can exercise
/// exactly the same checks as `mote serve`; production values are constructed
/// by [`serve`] from the bound listener and a newly generated token.
#[derive(Debug, Clone)]
pub struct ServerSecurity {
    port: u16,
    token: Arc<str>,
}

/// Shared process state. One instance is constructed per `mote serve`
/// process, then cheaply cloned into connection workers.
#[derive(Clone)]
pub struct ServerContext {
    store: Store,
    security: ServerSecurity,
    snapshot: SnapshotCache,
}

impl ServerContext {
    pub fn new(store: Store, security: ServerSecurity) -> MoteResult<Self> {
        let snapshot = SnapshotCache::new(&store)?;
        Ok(Self {
            store,
            security,
            snapshot,
        })
    }

    pub fn snapshot(&self) -> Arc<State> {
        self.snapshot.load()
    }
}

#[derive(Clone)]
struct SnapshotCache {
    state: Arc<RwLock<Arc<State>>>,
    subscribers: Arc<Mutex<BTreeMap<usize, SyncSender<EventEnvelope>>>>,
    next_subscriber: Arc<AtomicUsize>,
}

impl SnapshotCache {
    fn new(store: &Store) -> MoteResult<Self> {
        // EventTailer records the initial op set before the first replay, then
        // installs the process's only filesystem watcher. Its immediate poll
        // in the hub closes the installation race.
        let mut tailer = EventTailer::new(store, None, 1)?;
        let state = Arc::new(RwLock::new(Arc::new(reducer::replay_store(store)?)));
        let weak_state = Arc::downgrade(&state);
        let subscribers = Arc::new(Mutex::new(BTreeMap::new()));
        let weak_subscribers = Arc::downgrade(&subscribers);
        tailer.start(store)?;
        let watched_store = store.clone();
        std::thread::Builder::new()
            .name("mote-console-hub".into())
            .spawn(move || run_hub(tailer, weak_state, weak_subscribers, watched_store))?;
        Ok(Self {
            state,
            subscribers,
            next_subscriber: Arc::new(AtomicUsize::new(1)),
        })
    }

    fn load(&self) -> Arc<State> {
        self.state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn replace(&self, state: State) {
        *self
            .state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::new(state);
    }

    fn subscribe(&self) -> (usize, Receiver<EventEnvelope>) {
        let id = self.next_subscriber.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = sync_channel(256);
        self.subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id, sender);
        (id, receiver)
    }

    fn unsubscribe(&self, id: usize) {
        self.subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&id);
    }
}

fn run_hub(
    mut tailer: EventTailer,
    state: Weak<RwLock<Arc<State>>>,
    subscribers: Weak<Mutex<BTreeMap<usize, SyncSender<EventEnvelope>>>>,
    store: Store,
) {
    let filter = EventFilter::default();
    loop {
        let (Some(state), Some(subscribers)) = (state.upgrade(), subscribers.upgrade()) else {
            break;
        };
        match tailer.poll(&store, &filter) {
            Ok(events) if !events.is_empty() => match reducer::replay_store(&store) {
                Ok(fresh) => {
                    *state
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::new(fresh);
                    broadcast_events(&subscribers, events);
                }
                Err(error) => eprintln!("mote console snapshot refresh failed: {error}"),
            },
            Ok(_) => {}
            Err(error) => eprintln!("mote console event poll failed: {error}"),
        }
        if !tailer.wait() {
            break;
        }
    }
}

fn broadcast_events(
    subscribers: &Mutex<BTreeMap<usize, SyncSender<EventEnvelope>>>,
    events: Vec<EventEnvelope>,
) {
    let mut subscribers = subscribers
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for event in events {
        subscribers.retain(|_, sender| match sender.try_send(event.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
        });
    }
}

impl ServerSecurity {
    pub fn new(port: u16, token: impl Into<String>) -> Self {
        Self {
            port,
            token: Arc::from(token.into()),
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }
}

pub fn serve(store: Store, port: u16) -> MoteResult<()> {
    // The port is configurable, but the host deliberately is not: serving a
    // store to the network is outside the console's single-user model.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).map_err(|error| {
        MoteError::Invalid(format!(
            "cannot bind console to 127.0.0.1:{port}: {error}; choose another --port"
        ))
    })?;
    let port = listener.local_addr()?.port();
    let token = create_launch_token(&store)?;
    let security = ServerSecurity::new(port, token);
    let context = ServerContext::new(store, security)?;
    eprintln!(
        "mote console listening on http://127.0.0.1:{port}/?t={}",
        context.security.token()
    );
    serve_listener(listener, context)
}

fn serve_listener(listener: TcpListener, context: ServerContext) -> MoteResult<()> {
    let active = Arc::new(AtomicUsize::new(0));
    for incoming in listener.incoming() {
        let mut stream = incoming?;
        let previous = active.fetch_add(1, Ordering::AcqRel);
        if previous >= MAX_CONNECTIONS {
            active.fetch_sub(1, Ordering::AcqRel);
            write_response(
                &mut stream,
                503,
                br#"{"message":"connection limit reached"}"#,
            )?;
            continue;
        }
        let active = Arc::clone(&active);
        let context = context.clone();
        std::thread::spawn(move || {
            let _ = serve_connection(stream, &context);
            active.fetch_sub(1, Ordering::AcqRel);
        });
    }
    Ok(())
}

pub fn serve_connection(mut stream: TcpStream, context: &ServerContext) -> MoteResult<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(error) => {
            let body = serde_json::to_vec(&serde_json::json!({"message": error.message}))?;
            return write_response(&mut stream, error.status, &body);
        }
    };
    let authentication = match authorize_request(&request, &context.security) {
        Ok(authentication) => authentication,
        Err(error) => {
            let body = serde_json::to_vec(&serde_json::json!({"message": error.message}))?;
            return write_response(&mut stream, error.status, &body);
        }
    };
    if request.method == "GET" && request.path == "/api/events" {
        return serve_event_stream(&mut stream, &request, context);
    }
    if request.method == "GET" && !request.path.starts_with("/api/") {
        return serve_console_asset(&mut stream, &request, context, authentication);
    }
    match handle_request(&request, context) {
        Ok(response) => write_response(&mut stream, response.status, &response.body),
        Err(error) => {
            let body = serde_json::to_vec(&error.body)?;
            write_response(&mut stream, error.status, &body)
        }
    }
}

fn serve_console_asset(
    stream: &mut TcpStream,
    request: &Request,
    context: &ServerContext,
    authentication: Authentication,
) -> MoteResult<()> {
    match request.path.as_str() {
        "/console.js" => write_typed_response(
            stream,
            200,
            "text/javascript; charset=utf-8",
            CONSOLE_SCRIPT,
        ),
        "/console.css" => {
            write_typed_response(stream, 200, "text/css; charset=utf-8", CONSOLE_STYLE)
        }
        _ => {
            let index = String::from_utf8_lossy(CONSOLE_INDEX)
                .replace(
                    "<head>",
                    "<head>\n    <script>window.__MOTE_LIVE__ = true; const u = new URL(location.href); u.searchParams.delete('t'); history.replaceState(null, '', u.pathname + u.search + u.hash);</script>",
                );
            if authentication == Authentication::BootstrapQuery {
                let cookie = format!(
                    "{CONSOLE_COOKIE}={}; HttpOnly; SameSite=Strict; Path=/",
                    context.security.token()
                );
                write_typed_response_with_headers(
                    stream,
                    200,
                    "text/html; charset=utf-8",
                    &[("Set-Cookie", cookie.as_str())],
                    index.as_bytes(),
                )
            } else {
                write_typed_response(stream, 200, "text/html; charset=utf-8", index.as_bytes())
            }
        }
    }
}

fn serve_event_stream(
    stream: &mut TcpStream,
    request: &Request,
    context: &ServerContext,
) -> MoteResult<()> {
    let filter = EventFilter::default();
    let (subscriber_id, receiver) = context.snapshot.subscribe();
    let backlog = match request.headers.get("last-event-id") {
        Some(cursor) => match crate::events::accepted_events(&context.store, Some(cursor), &filter)
        {
            Ok(events) => events,
            Err(MoteError::Invalid(_) | MoteError::InvalidOpName(_)) => {
                context.snapshot.unsubscribe(subscriber_id);
                return write_response(
                    stream,
                    400,
                    br#"{"message":"invalid Last-Event-ID cursor"}"#,
                );
            }
            Err(error) => {
                context.snapshot.unsubscribe(subscriber_id);
                eprintln!("mote console SSE backlog failed: {error}");
                return write_response(stream, 500, br#"{"message":"store I/O failure"}"#);
            }
        },
        None => Vec::new(),
    };

    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nX-Accel-Buffering: no\r\nConnection: keep-alive\r\n\r\n"
    )?;
    stream.flush()?;

    let mut sent = BTreeSet::new();
    let result = (|| -> MoteResult<()> {
        for event in backlog {
            sent.insert(event.event_id.clone());
            write_sse_event(stream, &event)?;
        }
        loop {
            match receiver.recv_timeout(Duration::from_secs(15)) {
                Ok(event) => {
                    if sent.insert(event.event_id.clone()) {
                        write_sse_event(stream, &event)?;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    stream.write_all(b": heartbeat\n\n")?;
                    stream.flush()?;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        Ok(())
    })();
    context.snapshot.unsubscribe(subscriber_id);
    result
}

fn write_sse_event(stream: &mut TcpStream, event: &EventEnvelope) -> MoteResult<()> {
    let body = serde_json::to_string(event)?;
    write!(stream, "id: {}\ndata: {body}\n\n", event.event_id)?;
    stream.flush()?;
    Ok(())
}

struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

impl HttpResponse {
    fn json(status: u16, value: Value) -> Result<Self, ApiError> {
        let body = serde_json::to_vec(&value).map_err(ApiError::internal)?;
        Ok(Self { status, body })
    }

    fn empty(status: u16) -> Self {
        Self {
            status,
            body: Vec::new(),
        }
    }
}

#[derive(Debug)]
struct ApiError {
    status: u16,
    body: Value,
}

impl ApiError {
    fn message(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            body: json!({"message": message.into()}),
        }
    }

    fn conflict(op_id: String, reason: String, current: Value) -> Self {
        Self {
            status: 409,
            body: json!({"op_id": op_id, "reason": reason, "current": current}),
        }
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        eprintln!("mote console store failure: {error}");
        Self::message(500, "store I/O failure")
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateBeadInput {
    title: String,
    body: Option<String>,
    priority: Option<i32>,
    assignee: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    deps: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchBeadInput {
    fields: BTreeMap<String, Value>,
    clock: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimInput {
    ttl: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoteInput {
    kind: String,
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TagsInput {
    tag: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default = "default_true")]
    add: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DepInput {
    parent: String,
    #[serde(default = "default_blocks")]
    kind: String,
    #[serde(default = "default_true")]
    add: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseInput {
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReservationInput {
    issue: Option<String>,
    candidate: Option<String>,
    paths: Vec<String>,
    ttl: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnreserveInput {
    #[serde(default)]
    paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TopicInput {
    topic: String,
    title: Option<String>,
    body: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PostInput {
    body: String,
    reply_to: Option<String>,
    post_kind: Option<String>,
    #[serde(default)]
    answers: Vec<String>,
    #[serde(default)]
    notify: Vec<String>,
    idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StickyInput {
    sticky: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PromoteInput {
    title: String,
    body: String,
    priority: Option<i32>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    deps: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteInput {
    issue: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiscussionReadInput {
    topic: Option<String>,
    through: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageInput {
    to: String,
    body: String,
    kind: String,
    entity: Option<String>,
    reservation: Option<String>,
    idempotency_key: Option<String>,
    #[serde(default)]
    answers: Vec<String>,
    #[serde(default)]
    require_live: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyInput {
    body: String,
    kind: String,
    idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyInput {}

fn default_true() -> bool {
    true
}

fn default_blocks() -> String {
    "blocks".into()
}

fn handle_request(request: &Request, context: &ServerContext) -> Result<HttpResponse, ApiError> {
    if request.method == "GET" {
        if let Some(response) = handle_get(request, context)? {
            return Ok(response);
        }
    }

    if request.path == "/api/beads" {
        return match request.method.as_str() {
            "POST" => create_bead(request, context),
            _ => Err(ApiError::message(405, "method not allowed")),
        };
    }

    if let Some(rest) = request.path.strip_prefix("/api/beads/") {
        let segments: Vec<_> = rest.split('/').collect();
        return match (request.method.as_str(), segments.as_slice()) {
            ("PATCH", [id]) if !id.is_empty() => patch_bead(request, context, id),
            ("POST", [id, "notes"]) if !id.is_empty() => add_note(request, context, id),
            ("POST", [id, "tags"]) if !id.is_empty() => update_tags(request, context, id),
            ("POST", [id, "deps"]) if !id.is_empty() => update_dep(request, context, id),
            ("POST", [id, "claim"]) if !id.is_empty() => claim_bead(request, context, id),
            ("POST", [id, "release"]) if !id.is_empty() => release_bead(request, context, id),
            ("POST", [id, "close"]) if !id.is_empty() => close_bead(request, context, id),
            (_, [_, "notes" | "tags" | "deps" | "claim" | "release" | "close"]) | (_, [_]) => {
                Err(ApiError::message(405, "method not allowed"))
            }
            _ => Err(ApiError::message(404, "route not found")),
        };
    }

    if request.path == "/api/reservations" {
        return match request.method.as_str() {
            "POST" => create_reservation(request, context),
            _ => Err(ApiError::message(405, "method not allowed")),
        };
    }
    if let Some(reservation_id) = request.path.strip_prefix("/api/reservations/") {
        return if !reservation_id.is_empty()
            && !reservation_id.contains('/')
            && request.method == "DELETE"
        {
            close_reservation(request, context, reservation_id)
        } else if !reservation_id.is_empty() && !reservation_id.contains('/') {
            Err(ApiError::message(405, "method not allowed"))
        } else {
            Err(ApiError::message(404, "route not found"))
        };
    }

    if request.path == "/api/topics" {
        return match request.method.as_str() {
            "POST" => create_topic(request, context),
            _ => Err(ApiError::message(405, "method not allowed")),
        };
    }
    if let Some(rest) = request.path.strip_prefix("/api/topics/") {
        let segments: Vec<_> = rest.split('/').collect();
        return match (request.method.as_str(), segments.as_slice()) {
            ("POST", [topic, "posts"]) if !topic.is_empty() => create_post(request, context, topic),
            (_, [_, "posts"]) => Err(ApiError::message(405, "method not allowed")),
            _ => Err(ApiError::message(404, "route not found")),
        };
    }

    if let Some(rest) = request.path.strip_prefix("/api/posts/") {
        let segments: Vec<_> = rest.split('/').collect();
        return match (request.method.as_str(), segments.as_slice()) {
            ("POST", [post_id, "sticky"]) if !post_id.is_empty() => {
                set_post_sticky(request, context, post_id)
            }
            ("POST", [post_id, "promote"]) if !post_id.is_empty() => {
                promote_post(request, context, post_id)
            }
            ("POST", [post_id, "route"]) if !post_id.is_empty() => {
                route_post(request, context, post_id)
            }
            ("POST", [post_id, "needs-bead"]) if !post_id.is_empty() => {
                set_post_route(request, context, post_id, "needs_bead")
            }
            ("POST", [post_id, "resolve"]) if !post_id.is_empty() => {
                set_post_route(request, context, post_id, "resolved")
            }
            (_, [_, "sticky" | "promote" | "route" | "needs-bead" | "resolve"]) => {
                Err(ApiError::message(405, "method not allowed"))
            }
            _ => Err(ApiError::message(404, "route not found")),
        };
    }

    if request.path == "/api/discussion/read" {
        return match request.method.as_str() {
            "POST" => mark_discussion_read(request, context),
            _ => Err(ApiError::message(405, "method not allowed")),
        };
    }

    if request.path == "/api/messages" {
        return match request.method.as_str() {
            "POST" => send_message(request, context),
            _ => Err(ApiError::message(405, "method not allowed")),
        };
    }
    if let Some(rest) = request.path.strip_prefix("/api/messages/") {
        let segments: Vec<_> = rest.split('/').collect();
        return match (request.method.as_str(), segments.as_slice()) {
            ("POST", [message_id, "ack"]) if !message_id.is_empty() => {
                ack_message(request, context, message_id)
            }
            ("POST", [message_id, "reply"]) if !message_id.is_empty() => {
                reply_message(request, context, message_id)
            }
            ("POST", [message_id, "resolve"]) if !message_id.is_empty() => {
                resolve_message(request, context, message_id)
            }
            (_, [_, "ack" | "reply" | "resolve"]) => {
                Err(ApiError::message(405, "method not allowed"))
            }
            _ => Err(ApiError::message(404, "route not found")),
        };
    }

    if is_known_read_route(&request.path) {
        return Err(ApiError::message(405, "method not allowed"));
    }

    Err(ApiError::message(404, "route not found"))
}

fn is_known_read_route(path: &str) -> bool {
    matches!(
        path,
        "/api/health"
            | "/api/board"
            | "/api/beads"
            | "/api/topics"
            | "/api/unread"
            | "/api/unrouted"
            | "/api/search"
            | "/api/actors"
            | "/api/inflight"
            | "/api/events"
    ) || path.starts_with("/api/topics/")
        || path.starts_with("/api/posts/")
        || path.starts_with("/api/dm/")
        || path
            .strip_prefix("/api/beads/")
            .is_some_and(|rest| rest.ends_with("/history"))
}

fn handle_get(
    request: &Request,
    context: &ServerContext,
) -> Result<Option<HttpResponse>, ApiError> {
    if request.path == "/api/health" {
        let format = context.store.read_format().map_err(ApiError::internal)?;
        return HttpResponse::json(200, json!({"ok": true, "store_id": format.store_id})).map(Some);
    }

    let state = context.snapshot();
    let value = match request.path.as_str() {
        "/api/board" => board_json(&state, request_actor(request)?)?,
        "/api/beads" => beads_json(&state, request)?,
        "/api/topics" => Value::Array(
            state
                .board_topics_by_activity()
                .into_iter()
                .map(crate::cli::topic_json)
                .collect(),
        ),
        "/api/unread" => Value::Array(
            state
                .unread_board_posts_for(&request_actor(request)?, None)
                .into_iter()
                .map(crate::cli::board_post_json)
                .collect(),
        ),
        "/api/unrouted" => unrouted_json(&state, request)?,
        "/api/search" => search_json(&state, request)?,
        "/api/actors" => actors_json(&state, &request_actor(request)?)?,
        "/api/inflight" => inflight_json(&state, &request_actor(request)?, request)?,
        _ => {
            if let Some(rest) = request.path.strip_prefix("/api/beads/") {
                let segments: Vec<_> = rest.split('/').collect();
                match segments.as_slice() {
                    [id] if !id.is_empty() => bead_detail_json(&state, id)?,
                    [id, "history"] if !id.is_empty() => history_json(&state, id)?,
                    _ => return Ok(None),
                }
            } else if let Some(rest) = request.path.strip_prefix("/api/topics/") {
                let segments: Vec<_> = rest.split('/').collect();
                match segments.as_slice() {
                    [topic, "posts"] if !topic.is_empty() => {
                        if !state.board_topics.contains_key(*topic) {
                            return Err(ApiError::message(
                                404,
                                format!("no such discussion topic {topic}"),
                            ));
                        }
                        Value::Array(
                            state
                                .board_posts_for(Some(topic))
                                .into_iter()
                                .map(crate::cli::board_post_json)
                                .collect(),
                        )
                    }
                    _ => return Ok(None),
                }
            } else if let Some(rest) = request.path.strip_prefix("/api/posts/") {
                let segments: Vec<_> = rest.split('/').collect();
                match segments.as_slice() {
                    [post_id, "thread"] if !post_id.is_empty() => {
                        if !state.board_posts.contains_key(*post_id) {
                            return Err(ApiError::message(404, format!("no such post {post_id}")));
                        }
                        Value::Array(
                            state
                                .thread_posts(post_id)
                                .into_iter()
                                .map(|(depth, post)| {
                                    let mut value = crate::cli::board_post_json(post);
                                    value["depth"] = json!(depth);
                                    value
                                })
                                .collect(),
                        )
                    }
                    _ => return Ok(None),
                }
            } else if let Some(peer) = request.path.strip_prefix("/api/dm/") {
                if peer.is_empty() || peer.contains('/') {
                    return Ok(None);
                }
                let actor = request_actor(request)?;
                Value::Array(
                    state
                        .conversation_between(&actor, peer)
                        .into_iter()
                        .map(|message| crate::cli::thread_message_json(message, &actor))
                        .collect(),
                )
            } else {
                return Ok(None);
            }
        }
    };
    HttpResponse::json(200, value).map(Some)
}

fn query_bool(request: &Request, name: &str) -> Result<bool, ApiError> {
    match request.query_first(name) {
        None => Ok(false),
        Some("1" | "true") => Ok(true),
        Some("0" | "false") => Ok(false),
        Some(value) => Err(ApiError::message(
            422,
            format!("{name} must be 0, 1, false, or true (got `{value}`)"),
        )),
    }
}

fn beads_json(state: &State, request: &Request) -> Result<Value, ApiError> {
    let status = request
        .query_first("status")
        .map(|value| {
            Status::parse(value)
                .ok_or_else(|| ApiError::message(422, format!("invalid status: {value}")))
        })
        .transpose()?;
    let all = query_bool(request, "all")?;
    let ready = query_bool(request, "ready")?;
    let actor = request_actor(request).unwrap_or_default();
    let now = ids::format_rfc3339(jiff::Timestamp::now());
    let tags: Vec<&str> = request.query_all("tag").collect();
    let assignee = request.query_first("assignee");
    let mut beads: Vec<_> = state
        .live_beads()
        .filter(|bead| {
            (all || bead.status != Status::Closed || status.is_some())
                && status.is_none_or(|wanted| bead.status == wanted)
                && tags.iter().all(|tag| bead.tags.contains(*tag))
                && assignee.is_none_or(|wanted| bead.assignee.as_deref() == Some(wanted))
                && (!ready
                    || (state.is_ready(bead)
                        && bead
                            .claim
                            .as_ref()
                            .is_none_or(|claim| !claim.is_live(&now) || claim.claimed_by == actor)))
        })
        .collect();
    beads.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(Value::Array(beads.into_iter().map(bead_row_json).collect()))
}

fn bead_row_json(bead: &crate::state::Bead) -> Value {
    json!({
        "id": bead.id,
        "title": bead.title,
        "status": bead.status.as_str(),
        "priority": bead.priority,
        "tags": bead.tags.iter().collect::<Vec<_>>(),
        "assignee": bead.assignee,
    })
}

fn bead_detail_json(state: &State, id: &str) -> Result<Value, ApiError> {
    let bead = state
        .beads
        .get(id)
        .ok_or_else(|| ApiError::message(404, format!("no such bead {id}")))?;
    Ok(json!({
        "id": bead.id,
        "title": bead.title,
        "status": bead.status.as_str(),
        "priority": bead.priority,
        "body": bead.body,
        "assignee": bead.assignee,
        "tags": bead.tags.iter().collect::<Vec<_>>(),
        "deps": bead.deps.iter().map(|(parent, kind)| json!({"parent": parent, "kind": kind})).collect::<Vec<_>>(),
        "relations": bead.rels.iter().map(|(parent, kind)| json!({"parent": parent, "kind": kind})).collect::<Vec<_>>(),
        "children": state.relation_children_of(id).iter().map(|(child, kind)| crate::cli::bead_edge_json(child, kind)).collect::<Vec<_>>(),
        "dependents": state.dependency_children_of(id).iter().map(|(child, kind)| crate::cli::bead_edge_json(child, kind)).collect::<Vec<_>>(),
        "notes": bead.notes.iter().map(|note| json!({
            "op_id": note.op_id, "kind": note.note_kind, "actor": note.actor,
            "ts": note.ts, "text": note.text,
        })).collect::<Vec<_>>(),
        "discussion_sources": crate::cli::discussion_sources_json(state, id),
        "ready": state.is_ready(bead),
        "deleted_at": bead.deleted_at_ts,
        "created_at": bead.created_at_ts,
        "clock": bead.clock,
    }))
}

fn history_json(state: &State, id: &str) -> Result<Value, ApiError> {
    let entries = state
        .history
        .get(id)
        .ok_or_else(|| ApiError::message(404, format!("no such bead {id}")))?;
    Ok(Value::Array(
        entries
            .iter()
            .map(|entry| {
                json!({
                    "op_id": entry.op_id,
                    "kind": entry.kind,
                    "actor": entry.actor,
                    "ts": entry.ts,
                    "accepted": entry.accepted,
                    "reason": entry.reason,
                })
            })
            .collect(),
    ))
}

fn board_json(state: &State, actor: String) -> Result<Value, ApiError> {
    let as_of = jiff::Timestamp::now();
    let now = ids::format_rfc3339(as_of);
    let actors = crate::actor_status::actor_statuses(
        state,
        Some(&actor),
        as_of,
        crate::actor_status::DEFAULT_RECENT_WINDOW_S,
    );
    let mut status_counts = BTreeMap::<String, usize>::new();
    for bead in state.live_beads() {
        *status_counts
            .entry(bead.status.as_str().to_string())
            .or_insert(0) += 1;
    }
    let active_claims: Vec<_> = state
        .live_beads()
        .filter(|bead| {
            state.claim_disposition(bead, &now) == crate::state::LeaseDisposition::Active
        })
        .collect();
    let orphaned_claims: Vec<_> = state
        .beads
        .values()
        .filter(|bead| {
            state.claim_disposition(bead, &now) == crate::state::LeaseDisposition::Orphaned
        })
        .collect();
    let active_reservations: Vec<_> = state
        .reservations
        .values()
        .filter(|reservation| {
            state.reservation_disposition(reservation, &now)
                == crate::state::LeaseDisposition::Active
        })
        .collect();
    let orphaned_reservations: Vec<_> = state
        .reservations
        .values()
        .filter(|reservation| {
            state.reservation_disposition(reservation, &now)
                == crate::state::LeaseDisposition::Orphaned
        })
        .collect();
    let expiring_reservations: Vec<_> = state
        .reservations
        .values()
        .filter(|reservation| {
            state.reservation_expiry_phase(reservation, &now)
                == Some(crate::state::ReservationExpiryPhase::Expiring)
        })
        .collect();
    let expired_reservations: Vec<_> = state
        .reservations
        .values()
        .filter(|reservation| {
            state.reservation_expiry_phase(reservation, &now)
                == Some(crate::state::ReservationExpiryPhase::Expired)
        })
        .collect();
    Ok(json!({
        "actor": actor,
        "as_of_ts": now,
        "status_counts": status_counts,
        "active_claims": active_claims.iter().map(|bead| json!({
            "id": bead.id, "title": bead.title, "status": bead.status.as_str(),
            "claimed_by": bead.claim.as_ref().map(|claim| &claim.claimed_by),
            "lease_until_ts": bead.claim.as_ref().map(|claim| &claim.lease_until_ts),
        })).collect::<Vec<_>>(),
        "active_reservations": active_reservations.iter().map(|reservation| json!({
            "reservation_id": reservation.reservation_id, "actor": reservation.actor,
            "entity": reservation.entity, "binding_kind": state.reservation_binding_kind(reservation),
            "paths": reservation.live_paths(), "lease_until_ts": reservation.lease_until_ts,
        })).collect::<Vec<_>>(),
        "orphaned_claims": orphaned_claims.iter().map(|bead| json!({
            "id": bead.id, "title": bead.title,
            "claimed_by": bead.claim.as_ref().map(|claim| &claim.claimed_by),
            "lease_until_ts": bead.claim.as_ref().map(|claim| &claim.lease_until_ts),
            "disposition": "orphaned",
        })).collect::<Vec<_>>(),
        "orphaned_reservations": orphaned_reservations.iter().map(|reservation| json!({
            "reservation_id": reservation.reservation_id, "actor": reservation.actor,
            "entity": reservation.entity, "binding_kind": state.reservation_binding_kind(reservation),
            "paths": reservation.live_paths(), "lease_until_ts": reservation.lease_until_ts,
            "clock": reservation.clock, "disposition": "orphaned", "adoptions": reservation.adoptions,
        })).collect::<Vec<_>>(),
        "expiring_reservations": expiring_reservations.iter().map(|reservation| json!({
            "reservation_id": reservation.reservation_id, "holder": reservation.actor,
            "entity": reservation.entity, "binding_kind": state.reservation_binding_kind(reservation),
            "paths": reservation.live_paths(), "deadline": reservation.lease_until_ts,
            "reason": "ttl_near_deadline", "warning_at": state.reservation_warning_ts(reservation),
        })).collect::<Vec<_>>(),
        "expired_reservations": expired_reservations.iter().map(|reservation| json!({
            "reservation_id": reservation.reservation_id, "holder": reservation.actor,
            "entity": reservation.entity, "binding_kind": state.reservation_binding_kind(reservation),
            "paths": reservation.live_paths(), "deadline": reservation.lease_until_ts,
            "reason": "ttl_elapsed",
        })).collect::<Vec<_>>(),
        "inbox_unacked": state.inbox_for(&actor).len(),
        "discussion_unread": state.unread_board_posts_for(&actor, None).len(),
        "actors": actors,
    }))
}

fn unrouted_json(state: &State, request: &Request) -> Result<Value, ApiError> {
    let topic = request
        .query_first("topic")
        .map(crate::cli::normalize_discussion_topic)
        .transpose()
        .map_err(|error| ApiError::message(422, error.to_string()))?;
    Ok(json!({
        "topics": state.unrouted_topics(topic.as_deref()).into_iter().map(crate::cli::topic_json).collect::<Vec<_>>(),
        "posts": state.unrouted_posts(topic.as_deref()).into_iter().map(crate::cli::board_post_json).collect::<Vec<_>>(),
    }))
}

fn search_json(state: &State, request: &Request) -> Result<Value, ApiError> {
    let query = request.query_first("q").unwrap_or_default().trim();
    if query.is_empty() {
        return Err(ApiError::message(422, "search query must be non-empty"));
    }
    let topic = request
        .query_first("topic")
        .map(crate::cli::normalize_discussion_topic)
        .transpose()
        .map_err(|error| ApiError::message(422, error.to_string()))?;
    let limit = request
        .query_first("limit")
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| ApiError::message(422, "limit must be a non-negative integer"))
        })
        .transpose()?;
    let needle = query.to_ascii_lowercase();
    let matches = |value: &str| value.to_ascii_lowercase().contains(&needle);
    let mut topics: Vec<_> = state
        .board_topics_by_activity()
        .into_iter()
        .filter(|record| {
            topic.as_deref().is_none_or(|wanted| record.topic == wanted)
                && (matches(&record.topic) || matches(&record.title) || matches(&record.body))
        })
        .collect();
    let mut posts: Vec<_> = state
        .board_posts_for(topic.as_deref())
        .into_iter()
        .filter(|post| {
            matches(&post.post_id)
                || matches(&post.topic)
                || matches(&post.from)
                || matches(&post.body)
        })
        .collect();
    if let Some(limit) = limit {
        topics.truncate(limit);
        posts = crate::cli::limit_board_posts_preserving_stickies(posts, limit);
    }
    Ok(json!({
        "topics": topics.into_iter().map(crate::cli::topic_json).collect::<Vec<_>>(),
        "posts": posts.into_iter().map(crate::cli::board_post_json).collect::<Vec<_>>(),
    }))
}

fn actors_json(state: &State, viewer: &str) -> Result<Value, ApiError> {
    let summaries = crate::cli::actor_summaries(
        state,
        Some(viewer),
        jiff::Timestamp::now(),
        crate::actor_status::DEFAULT_RECENT_WINDOW_S,
    );
    let mut values = serde_json::to_value(summaries).map_err(ApiError::internal)?;
    for value in values.as_array_mut().into_iter().flatten() {
        let actor = value["actor"].as_str().unwrap_or_default();
        let last = state.conversation_between(viewer, actor).into_iter().last();
        value["last_message"] = last.map_or(Value::Null, |message| {
            json!({
                "body": message.body,
                "ts": message.sent_ts,
                "direction": if message.from == viewer { "out" } else { "in" },
            })
        });
    }
    Ok(values)
}

fn inflight_json(state: &State, actor: &str, request: &Request) -> Result<Value, ApiError> {
    let minutes = request
        .query_first("minutes")
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| ApiError::message(422, "minutes must be a non-negative integer"))
        })
        .transpose()?
        .unwrap_or(60);
    let now = jiff::Timestamp::now();
    let now_ts = ids::format_rfc3339(now);
    let window_secs = minutes.saturating_mul(60).min(i64::MAX as u64) as i64;
    let cutoff = now
        .checked_sub(jiff::SignedDuration::from_secs(window_secs))
        .map(ids::format_rfc3339)
        .unwrap_or_default();
    let sessions = state.live_sessions(&now_ts);
    let reservations: Vec<_> = state
        .reservations
        .values()
        .filter(|reservation| {
            state.reservation_disposition(reservation, &now_ts)
                == crate::state::LeaseDisposition::Active
        })
        .collect();
    let orphaned_reservations: Vec<_> = state
        .reservations
        .values()
        .filter(|reservation| {
            state.reservation_disposition(reservation, &now_ts)
                == crate::state::LeaseDisposition::Orphaned
        })
        .collect();
    let expiring_reservations: Vec<_> = state
        .reservations
        .values()
        .filter(|reservation| {
            state.reservation_expiry_phase(reservation, &now_ts)
                == Some(crate::state::ReservationExpiryPhase::Expiring)
        })
        .collect();
    let expired_reservations: Vec<_> = state
        .reservations
        .values()
        .filter(|reservation| {
            state.reservation_expiry_phase(reservation, &now_ts)
                == Some(crate::state::ReservationExpiryPhase::Expired)
        })
        .collect();
    let doing: Vec<_> = state
        .live_beads()
        .filter(|bead| bead.status == Status::Doing)
        .collect();
    let claims: Vec<_> = state
        .live_beads()
        .filter(|bead| {
            state.claim_disposition(bead, &now_ts) == crate::state::LeaseDisposition::Active
        })
        .collect();
    let orphaned_claims: Vec<_> = state
        .beads
        .values()
        .filter(|bead| {
            state.claim_disposition(bead, &now_ts) == crate::state::LeaseDisposition::Orphaned
        })
        .collect();
    let topics: Vec<_> = state
        .board_topics_by_activity()
        .into_iter()
        .filter(|topic| topic.last_activity_ts.as_str() >= cutoff.as_str())
        .collect();
    let candidates: Vec<_> = state.candidates.values().collect();
    let actors = crate::actor_status::actor_statuses(
        state,
        Some(actor),
        now,
        window_secs.max(0).min(u32::MAX as i64) as u32,
    );

    Ok(json!({
        "actor": actor,
        "now_ts": now_ts,
        "window_minutes": minutes,
        "sessions": sessions.iter().map(|session| crate::cli::session_json(session, &now_ts)).collect::<Vec<_>>(),
        "reservations": reservations.iter().map(|reservation| json!({
            "reservation_id": reservation.reservation_id, "actor": reservation.actor,
            "entity": reservation.entity, "binding_kind": state.reservation_binding_kind(reservation),
            "paths": reservation.live_paths(), "lease_until_ts": reservation.lease_until_ts,
        })).collect::<Vec<_>>(),
        "doing": doing.iter().map(|bead| json!({
            "id": bead.id, "title": bead.title, "priority": bead.priority,
            "claimed_by": bead.claim.as_ref().filter(|claim| claim.is_live(&now_ts)).map(|claim| &claim.claimed_by),
            "lease_until_ts": bead.claim.as_ref().filter(|claim| claim.is_live(&now_ts)).map(|claim| &claim.lease_until_ts),
        })).collect::<Vec<_>>(),
        "claims": claims.iter().map(|bead| json!({
            "id": bead.id, "status": bead.status.as_str(),
            "claimed_by": bead.claim.as_ref().map(|claim| &claim.claimed_by),
            "lease_until_ts": bead.claim.as_ref().map(|claim| &claim.lease_until_ts),
        })).collect::<Vec<_>>(),
        "orphaned_claims": orphaned_claims.iter().map(|bead| json!({
            "id": bead.id, "status": bead.status.as_str(),
            "claimed_by": bead.claim.as_ref().map(|claim| &claim.claimed_by),
            "lease_until_ts": bead.claim.as_ref().map(|claim| &claim.lease_until_ts),
            "disposition": "orphaned",
        })).collect::<Vec<_>>(),
        "orphaned_reservations": orphaned_reservations.iter().map(|reservation| json!({
            "reservation_id": reservation.reservation_id, "actor": reservation.actor,
            "entity": reservation.entity, "binding_kind": state.reservation_binding_kind(reservation),
            "paths": reservation.live_paths(), "lease_until_ts": reservation.lease_until_ts,
            "clock": reservation.clock, "disposition": "orphaned", "adoptions": reservation.adoptions,
        })).collect::<Vec<_>>(),
        "expiring_reservations": expiring_reservations.iter().map(|reservation| json!({
            "reservation_id": reservation.reservation_id, "holder": reservation.actor,
            "entity": reservation.entity, "binding_kind": state.reservation_binding_kind(reservation),
            "paths": reservation.live_paths(), "warning_at": state.reservation_warning_ts(reservation),
            "deadline": reservation.lease_until_ts, "reason": "ttl_near_deadline",
        })).collect::<Vec<_>>(),
        "expired_reservations": expired_reservations.iter().map(|reservation| json!({
            "reservation_id": reservation.reservation_id, "holder": reservation.actor,
            "entity": reservation.entity, "binding_kind": state.reservation_binding_kind(reservation),
            "paths": reservation.live_paths(), "deadline": reservation.lease_until_ts,
            "reason": "ttl_elapsed",
        })).collect::<Vec<_>>(),
        "topics": topics.iter().map(|topic| {
            let mut value = crate::cli::topic_json(topic);
            value["unread"] = json!(state.unread_board_posts_for(actor, Some(&topic.topic)).len());
            value
        }).collect::<Vec<_>>(),
        "candidates": candidates.iter().map(|candidate| {
            let mut value = crate::cli::candidate_json(state, candidate);
            value["landability"] = json!(state.candidate_landability(&candidate.candidate_id, Some(actor)));
            value
        }).collect::<Vec<_>>(),
        "recent_commits_advisory": Vec::<Value>::new(),
        "actors": actors,
    }))
}

fn request_actor(request: &Request) -> Result<String, ApiError> {
    let actor = request
        .headers
        .get("x-mote-actor")
        .ok_or_else(|| ApiError::message(400, "missing X-Mote-Actor"))?;
    crate::cli::normalize_actor(actor).map_err(|error| ApiError::message(400, error.to_string()))
}

fn json_input<T: for<'de> Deserialize<'de>>(request: &Request) -> Result<T, ApiError> {
    let value: Value = serde_json::from_slice(&request.body)
        .map_err(|error| ApiError::message(400, format!("malformed JSON: {error}")))?;
    serde_json::from_value(value)
        .map_err(|error| ApiError::message(422, format!("invalid input: {error}")))
}

fn create_bead(request: &Request, context: &ServerContext) -> Result<HttpResponse, ApiError> {
    let actor = request_actor(request)?;
    let input: CreateBeadInput = json_input(request)?;
    if input.title.trim().is_empty() {
        return Err(ApiError::message(422, "title must be non-empty"));
    }
    if input
        .priority
        .is_some_and(|priority| !(0..=3).contains(&priority))
    {
        return Err(ApiError::message(422, "priority must be in 0..=3"));
    }

    let id = ids::new_bead_id();
    let mut set = ScalarSet {
        title: Some(input.title),
        priority: input.priority,
        body: input.body,
        assignee: input.assignee,
        ..Default::default()
    };
    set.status = Some(Status::Open);
    let mut ops = vec![op::make_create(
        actor.clone(),
        id.clone(),
        set,
        jiff::Timestamp::now(),
    )];
    ops.extend(
        input
            .tags
            .into_iter()
            .map(|tag| op::make_tag(true, actor.clone(), id.clone(), tag, jiff::Timestamp::now())),
    );
    ops.extend(input.deps.into_iter().map(|parent| {
        op::make_dep(
            true,
            actor.clone(),
            id.clone(),
            parent,
            "blocks".into(),
            jiff::Timestamp::now(),
        )
    }));
    publish_and_verify(context, &ops, Some(&id))?;
    HttpResponse::json(201, json!({"id": id}))
}

fn patch_bead(
    request: &Request,
    context: &ServerContext,
    id: &str,
) -> Result<HttpResponse, ApiError> {
    let actor = request_actor(request)?;
    let input: PatchBeadInput = json_input(request)?;
    if input.fields.is_empty() {
        return Err(ApiError::message(422, "fields must be non-empty"));
    }
    if input
        .clock
        .keys()
        .any(|field| !input.fields.contains_key(field))
    {
        return Err(ApiError::message(
            422,
            "clock may only contain fields being patched",
        ));
    }
    let fresh = reducer::replay_store(&context.store).map_err(ApiError::internal)?;
    if fresh.beads.get(id).is_none_or(|bead| bead.is_deleted()) {
        return Err(ApiError::message(404, format!("no such bead {id}")));
    }

    let mut set = ScalarSet::default();
    for (field, value) in &input.fields {
        if !input.clock.contains_key(field) {
            return Err(ApiError::message(
                422,
                format!("missing observed clock for `{field}`"),
            ));
        }
        match field.as_str() {
            "title" => set.title = Some(value_string(value, field)?),
            "status" => {
                let status = value_string(value, field)?;
                set.status =
                    Some(Status::parse(&status).ok_or_else(|| {
                        ApiError::message(422, format!("invalid status: {status}"))
                    })?);
            }
            "priority" => {
                let priority = value.as_i64().ok_or_else(|| {
                    ApiError::message(422, "priority must be an integer in 0..=3")
                })?;
                if !(0..=3).contains(&priority) {
                    return Err(ApiError::message(422, "priority must be in 0..=3"));
                }
                set.priority = Some(priority as i32);
            }
            "body" => set.body = Some(value_string(value, field)?),
            "assignee" => set.assignee = Some(value_string(value, field)?),
            other => {
                return Err(ApiError::message(
                    422,
                    format!("unknown scalar field `{other}`"),
                ));
            }
        }
    }
    let operation = op::make_patch(
        actor,
        id.to_string(),
        input.clock,
        set,
        jiff::Timestamp::now(),
    );
    publish_and_verify(context, &[operation], Some(id))?;
    Ok(HttpResponse::empty(204))
}

fn claim_bead(
    request: &Request,
    context: &ServerContext,
    id: &str,
) -> Result<HttpResponse, ApiError> {
    let actor = request_actor(request)?;
    let input: ClaimInput = json_input(request)?;
    if input.ttl == 0 {
        return Err(ApiError::message(422, "ttl must be greater than zero"));
    }
    let fresh = reducer::replay_store(&context.store).map_err(ApiError::internal)?;
    let bead = fresh
        .beads
        .get(id)
        .filter(|bead| !bead.is_deleted())
        .ok_or_else(|| ApiError::message(404, format!("no such bead {id}")))?;
    let expect_claim = bead
        .claim
        .as_ref()
        .filter(|claim| claim.claimed_by == actor)
        .map(|claim| claim.claim_clock.clone());
    let operation = op::make_claim(
        actor.clone(),
        id.to_string(),
        actor,
        input.ttl,
        expect_claim,
        jiff::Timestamp::now(),
    );
    publish_and_verify(context, &[operation], Some(id))?;
    Ok(HttpResponse::empty(204))
}

fn add_note(
    request: &Request,
    context: &ServerContext,
    id: &str,
) -> Result<HttpResponse, ApiError> {
    let actor = request_actor(request)?;
    let input: NoteInput = json_input(request)?;
    if !op::validate_note_kind(&input.kind) {
        return Err(ApiError::message(422, "invalid note kind"));
    }
    if input.text.trim().is_empty() {
        return Err(ApiError::message(422, "note text must be non-empty"));
    }
    require_live_bead(context, id)?;
    let operation = op::make_note(
        actor,
        id.to_string(),
        input.kind,
        input.text,
        jiff::Timestamp::now(),
    );
    publish_and_verify(context, &[operation], Some(id))?;
    Ok(HttpResponse::empty(204))
}

fn update_tags(
    request: &Request,
    context: &ServerContext,
    id: &str,
) -> Result<HttpResponse, ApiError> {
    let actor = request_actor(request)?;
    let mut input: TagsInput = json_input(request)?;
    if let Some(tag) = input.tag.take() {
        input.tags.push(tag);
    }
    if input.tags.is_empty() || input.tags.iter().any(|tag| tag.trim().is_empty()) {
        return Err(ApiError::message(
            422,
            "at least one non-empty tag is required",
        ));
    }
    input.tags.sort();
    input.tags.dedup();
    require_live_bead(context, id)?;
    let operations: Vec<_> = input
        .tags
        .into_iter()
        .map(|tag| {
            op::make_tag(
                input.add,
                actor.clone(),
                id.to_string(),
                tag,
                jiff::Timestamp::now(),
            )
        })
        .collect();
    publish_and_verify(context, &operations, Some(id))?;
    Ok(HttpResponse::empty(204))
}

fn update_dep(
    request: &Request,
    context: &ServerContext,
    id: &str,
) -> Result<HttpResponse, ApiError> {
    let actor = request_actor(request)?;
    let input: DepInput = json_input(request)?;
    if input.parent.trim().is_empty() || input.kind.trim().is_empty() {
        return Err(ApiError::message(422, "parent and kind must be non-empty"));
    }
    require_live_bead(context, id)?;
    let operation = op::make_dep(
        input.add,
        actor,
        id.to_string(),
        input.parent,
        input.kind,
        jiff::Timestamp::now(),
    );
    publish_and_verify(context, &[operation], Some(id))?;
    Ok(HttpResponse::empty(204))
}

fn release_bead(
    request: &Request,
    context: &ServerContext,
    id: &str,
) -> Result<HttpResponse, ApiError> {
    let actor = request_actor(request)?;
    let _: EmptyInput = json_input(request)?;
    require_live_bead(context, id)?;
    let operation = op::make_release(actor, id.to_string(), None, jiff::Timestamp::now());
    publish_and_verify(context, &[operation], Some(id))?;
    Ok(HttpResponse::empty(204))
}

fn close_bead(
    request: &Request,
    context: &ServerContext,
    id: &str,
) -> Result<HttpResponse, ApiError> {
    let actor = request_actor(request)?;
    let input: CloseInput = json_input(request)?;
    if input
        .note
        .as_deref()
        .is_some_and(|note| note.trim().is_empty())
    {
        return Err(ApiError::message(
            422,
            "note must be non-empty when provided",
        ));
    }
    let fresh = require_live_bead(context, id)?;
    let bead = &fresh.beads[id];
    let mut expect = BTreeMap::new();
    if let Some(clock) = bead.clock.get("status") {
        expect.insert("status".to_string(), clock.clone());
    }
    let mut operations = Vec::new();
    if let Some(note) = input.note {
        operations.push(op::make_note(
            actor.clone(),
            id.to_string(),
            "note".into(),
            note,
            jiff::Timestamp::now(),
        ));
    }
    operations.push(op::make_close(
        actor,
        id.to_string(),
        expect,
        jiff::Timestamp::now(),
    ));
    publish_and_verify(context, &operations, Some(id))?;
    Ok(HttpResponse::empty(204))
}

fn require_live_bead(context: &ServerContext, id: &str) -> Result<State, ApiError> {
    let fresh = reducer::replay_store(&context.store).map_err(ApiError::internal)?;
    if fresh.beads.get(id).is_none_or(|bead| bead.is_deleted()) {
        return Err(ApiError::message(404, format!("no such bead {id}")));
    }
    Ok(fresh)
}

fn create_reservation(
    request: &Request,
    context: &ServerContext,
) -> Result<HttpResponse, ApiError> {
    let actor = request_actor(request)?;
    let input: ReservationInput = json_input(request)?;
    let entity = match (input.issue, input.candidate) {
        (Some(entity), None) | (None, Some(entity)) => entity,
        _ => {
            return Err(ApiError::message(
                422,
                "exactly one of issue or candidate is required",
            ));
        }
    };
    if input.paths.is_empty() {
        return Err(ApiError::message(422, "at least one path is required"));
    }
    let mut paths = input
        .paths
        .iter()
        .map(|path| {
            crate::paths::normalize(path)
                .map_err(|error| ApiError::message(422, format!("path `{path}`: {error}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    paths.dedup();
    let ttl = input.ttl.unwrap_or(
        context
            .store
            .read_format()
            .map_err(ApiError::internal)?
            .default_ttl_s
            .reservation,
    );
    if ttl == 0 {
        return Err(ApiError::message(422, "ttl must be greater than zero"));
    }
    let reservation_id = ids::new_reservation_id();
    let operation = op::make_reserve_open(
        actor,
        reservation_id.clone(),
        entity,
        paths,
        ttl,
        jiff::Timestamp::now(),
    );
    publish_and_verify(context, &[operation], None)?;
    HttpResponse::json(201, json!({"reservation_id": reservation_id}))
}

fn close_reservation(
    request: &Request,
    context: &ServerContext,
    reservation_id: &str,
) -> Result<HttpResponse, ApiError> {
    let actor = request_actor(request)?;
    let input: UnreserveInput = json_input(request)?;
    let fresh = reducer::replay_store(&context.store).map_err(ApiError::internal)?;
    if !fresh.reservations.contains_key(reservation_id) {
        return Err(ApiError::message(
            404,
            format!("no such reservation `{reservation_id}`"),
        ));
    }
    let paths = if input.paths.is_empty() {
        None
    } else {
        Some(
            input
                .paths
                .iter()
                .map(|path| {
                    crate::paths::normalize(path)
                        .map_err(|error| ApiError::message(422, format!("path `{path}`: {error}")))
                })
                .collect::<Result<Vec<_>, _>>()?,
        )
    };
    let operation = op::make_reserve_close(
        actor,
        reservation_id.to_string(),
        paths,
        jiff::Timestamp::now(),
    );
    publish_and_verify(context, &[operation], None)?;
    Ok(HttpResponse::empty(204))
}

fn create_topic(request: &Request, context: &ServerContext) -> Result<HttpResponse, ApiError> {
    let actor = request_actor(request)?;
    let input: TopicInput = json_input(request)?;
    let topic = crate::cli::normalize_discussion_topic(&input.topic)
        .map_err(|error| ApiError::message(422, error.to_string()))?;
    if input
        .body
        .as_deref()
        .is_some_and(|body| body.trim().is_empty())
    {
        return Err(ApiError::message(
            422,
            "initial post body must be non-empty when provided",
        ));
    }
    let title = input
        .title
        .map(|title| title.trim().to_string())
        .filter(|title| !title.is_empty());
    let mut operations = vec![op::make_board_topic(
        actor.clone(),
        topic.clone(),
        title,
        None,
        jiff::Timestamp::now(),
    )];
    if let Some(body) = input.body {
        operations.push(op::make_board_post(
            actor,
            ids::new_post_id(),
            topic.clone(),
            body,
            None,
            jiff::Timestamp::now(),
        ));
    }
    publish_and_verify(context, &operations, None)?;
    HttpResponse::json(201, json!({"topic": topic}))
}

fn create_post(
    request: &Request,
    context: &ServerContext,
    raw_topic: &str,
) -> Result<HttpResponse, ApiError> {
    let actor = request_actor(request)?;
    let input: PostInput = json_input(request)?;
    if input.body.trim().is_empty() {
        return Err(ApiError::message(422, "post body must be non-empty"));
    }
    if input
        .post_kind
        .as_deref()
        .is_some_and(|kind| !op::validate_post_kind(kind))
    {
        return Err(ApiError::message(422, "invalid post kind"));
    }
    validate_idempotency(input.idempotency_key.as_deref())?;
    let topic = crate::cli::normalize_discussion_topic(raw_topic)
        .map_err(|error| ApiError::message(422, error.to_string()))?;
    let mut notify = input
        .notify
        .iter()
        .map(|recipient| {
            crate::cli::normalize_actor(recipient)
                .map_err(|error| ApiError::message(422, error.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    notify.retain(|recipient| recipient != &actor);
    notify.sort();
    notify.dedup();
    if let Some(key) = input.idempotency_key.as_deref() {
        let fresh = reducer::replay_store(&context.store).map_err(ApiError::internal)?;
        if let Some(existing) = fresh.board_post_by_idempotency(&actor, key) {
            let wanted_kind = input.post_kind.as_deref().unwrap_or("post");
            if existing.topic == topic
                && existing.body == input.body
                && existing.reply_to == input.reply_to
                && existing.post_kind == wanted_kind
                && existing.answers == input.answers
                && existing.notification_recipients == notify
            {
                return HttpResponse::json(
                    200,
                    json!({"post_id": existing.post_id, "idempotent_replay": true}),
                );
            }
            return Err(ApiError::message(
                422,
                "idempotency key is already used by a different post",
            ));
        }
    }
    let post_id = ids::new_post_id();
    let operation = op::make_board_post_with_options(
        actor,
        post_id.clone(),
        topic,
        input.body,
        input.reply_to,
        input.post_kind,
        input.answers,
        notify,
        input.idempotency_key,
        jiff::Timestamp::now(),
    );
    publish_and_verify(context, &[operation], None)?;
    HttpResponse::json(201, json!({"post_id": post_id}))
}

fn set_post_sticky(
    request: &Request,
    context: &ServerContext,
    post_id: &str,
) -> Result<HttpResponse, ApiError> {
    let actor = request_actor(request)?;
    let input: StickyInput = json_input(request)?;
    require_post(context, post_id)?;
    let operation = op::make_board_sticky(
        actor,
        post_id.to_string(),
        input.sticky,
        jiff::Timestamp::now(),
    );
    publish_and_verify(context, &[operation], None)?;
    Ok(HttpResponse::empty(204))
}

fn promote_post(
    request: &Request,
    context: &ServerContext,
    post_id: &str,
) -> Result<HttpResponse, ApiError> {
    let actor = request_actor(request)?;
    let input: PromoteInput = json_input(request)?;
    if input.title.trim().is_empty() {
        return Err(ApiError::message(422, "title must be non-empty"));
    }
    if input
        .priority
        .is_some_and(|priority| !(0..=3).contains(&priority))
    {
        return Err(ApiError::message(422, "priority must be in 0..=3"));
    }
    require_post(context, post_id)?;
    let id = ids::new_bead_id();
    let mut set = ScalarSet {
        title: Some(input.title),
        body: Some(input.body),
        priority: input.priority,
        ..Default::default()
    };
    set.status = Some(Status::Open);
    let mut operations = vec![op::make_create(
        actor.clone(),
        id.clone(),
        set,
        jiff::Timestamp::now(),
    )];
    operations.extend(
        input
            .tags
            .into_iter()
            .map(|tag| op::make_tag(true, actor.clone(), id.clone(), tag, jiff::Timestamp::now())),
    );
    operations.extend(input.deps.into_iter().map(|parent| {
        op::make_dep(
            true,
            actor.clone(),
            id.clone(),
            parent,
            "blocks".into(),
            jiff::Timestamp::now(),
        )
    }));
    operations.push(op::make_board_route(
        actor,
        Some(post_id.to_string()),
        None,
        "routed".into(),
        Some(id.clone()),
        None,
        jiff::Timestamp::now(),
    ));
    publish_and_verify(context, &operations, Some(&id))?;
    HttpResponse::json(201, json!({"id": id}))
}

fn route_post(
    request: &Request,
    context: &ServerContext,
    post_id: &str,
) -> Result<HttpResponse, ApiError> {
    let actor = request_actor(request)?;
    let input: RouteInput = json_input(request)?;
    require_post(context, post_id)?;
    require_live_bead(context, &input.issue)?;
    let operation = op::make_board_route(
        actor,
        Some(post_id.to_string()),
        None,
        "routed".into(),
        Some(input.issue),
        None,
        jiff::Timestamp::now(),
    );
    publish_and_verify(context, &[operation], None)?;
    Ok(HttpResponse::empty(204))
}

fn set_post_route(
    request: &Request,
    context: &ServerContext,
    post_id: &str,
    route_state: &str,
) -> Result<HttpResponse, ApiError> {
    let actor = request_actor(request)?;
    let _: EmptyInput = json_input(request)?;
    require_post(context, post_id)?;
    let operation = op::make_board_route(
        actor,
        Some(post_id.to_string()),
        None,
        route_state.into(),
        None,
        None,
        jiff::Timestamp::now(),
    );
    publish_and_verify(context, &[operation], None)?;
    Ok(HttpResponse::empty(204))
}

fn require_post(context: &ServerContext, post_id: &str) -> Result<State, ApiError> {
    let fresh = reducer::replay_store(&context.store).map_err(ApiError::internal)?;
    if !fresh.board_posts.contains_key(post_id) {
        return Err(ApiError::message(404, format!("no such post {post_id}")));
    }
    Ok(fresh)
}

fn mark_discussion_read(
    request: &Request,
    context: &ServerContext,
) -> Result<HttpResponse, ApiError> {
    let actor = request_actor(request)?;
    let input: DiscussionReadInput = json_input(request)?;
    let topic = input
        .topic
        .as_deref()
        .map(crate::cli::normalize_discussion_topic)
        .transpose()
        .map_err(|error| ApiError::message(422, error.to_string()))?;
    let fresh = reducer::replay_store(&context.store).map_err(ApiError::internal)?;
    let target = if let Some(post_id) = input.through.as_deref() {
        let post = fresh
            .board_posts
            .get(post_id)
            .ok_or_else(|| ApiError::message(404, format!("no such post {post_id}")))?;
        if topic.as_deref().is_some_and(|wanted| wanted != post.topic) {
            return Err(ApiError::message(422, "post does not belong to topic"));
        }
        Some((post.sent_op_id.clone(), true))
    } else {
        fresh
            .board_posts_for(topic.as_deref())
            .into_iter()
            .max_by(|left, right| left.sent_op_id.cmp(&right.sent_op_id))
            .map(|post| (post.sent_op_id.clone(), false))
    };
    let Some((upto, strict)) = target else {
        return Ok(HttpResponse::empty(204));
    };
    let operation = if strict {
        op::make_board_read_through(actor, upto, topic, jiff::Timestamp::now())
    } else {
        op::make_board_read(actor, upto, topic, jiff::Timestamp::now())
    };
    publish_and_verify(context, &[operation], None)?;
    Ok(HttpResponse::empty(204))
}

fn send_message(request: &Request, context: &ServerContext) -> Result<HttpResponse, ApiError> {
    let actor = request_actor(request)?;
    let input: MessageInput = json_input(request)?;
    let to = crate::cli::normalize_actor(&input.to)
        .map_err(|error| ApiError::message(422, error.to_string()))?;
    if !op::validate_msg_kind(&input.kind) || op::VALID_REPLY_KINDS.contains(&input.kind.as_str()) {
        return Err(ApiError::message(422, "invalid root message kind"));
    }
    if input.body.trim().is_empty() {
        return Err(ApiError::message(422, "message body must be non-empty"));
    }
    validate_idempotency(input.idempotency_key.as_deref())?;
    if let Some(key) = input.idempotency_key.as_deref() {
        let fresh = reducer::replay_store(&context.store).map_err(ApiError::internal)?;
        if let Some(existing) = fresh.message_by_idempotency(&actor, key) {
            if existing.to == to
                && existing.entity == input.entity
                && existing.reservation == input.reservation
                && existing.msg_kind == input.kind
                && existing.body == input.body
                && existing.reply_to.is_none()
                && existing.answers == input.answers
                && existing.require_live == input.require_live
            {
                return HttpResponse::json(200, message_send_json(existing, true));
            }
            return Err(ApiError::message(
                422,
                "idempotency key is already used by a different message",
            ));
        }
    }
    let msg_id = ids::new_msg_id();
    let correlation_id = (input.kind == "request").then(|| msg_id.clone());
    let operation = op::make_msg_send_with_options(
        actor,
        msg_id.clone(),
        to,
        input.entity,
        input.reservation,
        input.kind,
        input.body,
        None,
        correlation_id,
        input.idempotency_key,
        input.answers,
        input.require_live,
        jiff::Timestamp::now(),
    );
    publish_and_verify(context, &[operation], None)?;
    let fresh = reducer::replay_store(&context.store).map_err(ApiError::internal)?;
    let message = fresh
        .messages
        .get(&msg_id)
        .ok_or_else(|| ApiError::internal("accepted send did not produce a message record"))?;
    HttpResponse::json(201, message_send_json(message, false))
}

fn message_send_json(message: &MsgRecord, idempotent_replay: bool) -> Value {
    json!({
        "accepted": true,
        "msg_id": message.msg_id,
        "delivery": "queued",
        "addressed": true,
        "private": false,
        "require_live": message.require_live,
        "idempotent_replay": idempotent_replay,
        "recipient": message.to,
        "recipient_presence": message.recipient_presence,
    })
}

fn ack_message(
    request: &Request,
    context: &ServerContext,
    message_id: &str,
) -> Result<HttpResponse, ApiError> {
    let actor = request_actor(request)?;
    let _: EmptyInput = json_input(request)?;
    require_message(context, message_id)?;
    let operation = op::make_msg_ack(actor, message_id.to_string(), jiff::Timestamp::now());
    publish_and_verify(context, &[operation], None)?;
    Ok(HttpResponse::empty(204))
}

fn reply_message(
    request: &Request,
    context: &ServerContext,
    message_id: &str,
) -> Result<HttpResponse, ApiError> {
    let actor = request_actor(request)?;
    let input: ReplyInput = json_input(request)?;
    if !op::VALID_REPLY_KINDS.contains(&input.kind.as_str()) {
        return Err(ApiError::message(
            422,
            "reply kind must be response or decline",
        ));
    }
    if input.body.trim().is_empty() {
        return Err(ApiError::message(422, "reply body must be non-empty"));
    }
    validate_idempotency(input.idempotency_key.as_deref())?;
    let fresh = require_message(context, message_id)?;
    let root = &fresh.messages[message_id];
    if root.reply_to.is_some() || root.msg_kind != "request" {
        return Err(ApiError::message(422, "message is not a root request"));
    }
    if root.to != actor {
        return Err(ApiError::message(
            422,
            "request is addressed to another actor",
        ));
    }
    if let Some(key) = input.idempotency_key.as_deref() {
        if let Some(existing) = fresh.message_by_idempotency(&actor, key) {
            if existing.to == root.from
                && existing.entity == root.entity
                && existing.reservation == root.reservation
                && existing.msg_kind == input.kind
                && existing.body == input.body
                && existing.reply_to.as_deref() == Some(message_id)
            {
                return HttpResponse::json(
                    200,
                    json!({"msg_id": existing.msg_id, "idempotent_replay": true}),
                );
            }
            return Err(ApiError::message(
                422,
                "idempotency key is already used by a different message",
            ));
        }
    }
    if root.request_state != Some(crate::state::RequestState::Open) {
        return Err(ApiError::message(422, "request is not open"));
    }
    let reply_id = ids::new_msg_id();
    let operation = op::make_msg_send_with_options(
        actor,
        reply_id.clone(),
        root.from.clone(),
        root.entity.clone(),
        root.reservation.clone(),
        input.kind,
        input.body,
        Some(message_id.to_string()),
        Some(
            root.correlation_id
                .clone()
                .unwrap_or_else(|| message_id.to_string()),
        ),
        input.idempotency_key,
        Vec::new(),
        false,
        jiff::Timestamp::now(),
    );
    publish_and_verify(context, &[operation], None)?;
    HttpResponse::json(201, json!({"msg_id": reply_id}))
}

fn resolve_message(
    request: &Request,
    context: &ServerContext,
    message_id: &str,
) -> Result<HttpResponse, ApiError> {
    let actor = request_actor(request)?;
    let _: EmptyInput = json_input(request)?;
    require_message(context, message_id)?;
    let operation = op::make_msg_resolve(actor, message_id.to_string(), jiff::Timestamp::now());
    publish_and_verify(context, &[operation], None)?;
    Ok(HttpResponse::empty(204))
}

fn require_message(context: &ServerContext, message_id: &str) -> Result<State, ApiError> {
    let fresh = reducer::replay_store(&context.store).map_err(ApiError::internal)?;
    if !fresh.messages.contains_key(message_id) {
        return Err(ApiError::message(
            404,
            format!("no such message `{message_id}`"),
        ));
    }
    Ok(fresh)
}

fn validate_idempotency(key: Option<&str>) -> Result<(), ApiError> {
    if key.is_some_and(|key| !op::validate_idempotency_key(key)) {
        return Err(ApiError::message(422, "invalid idempotency key"));
    }
    Ok(())
}

fn value_string(value: &Value, field: &str) -> Result<String, ApiError> {
    value
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| ApiError::message(422, format!("{field} must be a string")))
}

fn publish_and_verify(
    context: &ServerContext,
    ops: &[Op],
    entity: Option<&str>,
) -> Result<Vec<String>, ApiError> {
    let mut names = Vec::with_capacity(ops.len());
    for operation in ops {
        names.push(
            publish::publish_op(&context.store, operation)
                .map_err(ApiError::internal)?
                .into_string(),
        );
    }
    let fresh = reducer::replay_store(&context.store).map_err(ApiError::internal)?;
    let rejected = names.iter().find_map(|name| {
        (!fresh.was_accepted(name)).then(|| {
            (
                name.clone(),
                fresh
                    .rejection_reason(name)
                    .unwrap_or_else(|| "unknown reducer rejection".into()),
            )
        })
    });
    let current = entity
        .and_then(|id| fresh.beads.get(id))
        .map(current_bead_fields)
        .unwrap_or_else(|| Value::Object(Map::new()));
    context.snapshot.replace(fresh);
    if let Some((op_id, reason)) = rejected {
        return Err(ApiError::conflict(op_id, reason, current));
    }
    Ok(names)
}

fn current_bead_fields(bead: &crate::state::Bead) -> Value {
    json!({
        "title": bead.title,
        "status": bead.status.as_str(),
        "priority": bead.priority,
        "body": bead.body,
        "assignee": bead.assignee,
    })
}

#[derive(Debug)]
struct Request {
    method: String,
    path: String,
    query: BTreeMap<String, Vec<String>>,
    headers: BTreeMap<String, String>,
    #[allow(dead_code)]
    body: Vec<u8>,
}

impl Request {
    fn query_first(&self, name: &str) -> Option<&str> {
        self.query
            .get(name)
            .and_then(|values| values.first())
            .map(String::as_str)
    }

    fn query_all(&self, name: &str) -> impl Iterator<Item = &str> {
        self.query
            .get(name)
            .into_iter()
            .flatten()
            .map(String::as_str)
    }
}

#[derive(Debug)]
struct HttpError {
    status: u16,
    message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Authentication {
    Header,
    Cookie,
    BootstrapQuery,
}

fn read_request(stream: &mut TcpStream) -> Result<Request, HttpError> {
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        if bytes.len() >= MAX_HEADERS {
            return Err(http_error(431, "header block exceeds 32 KiB"));
        }
        let mut buffer = [0_u8; 4096];
        let count = stream
            .read(&mut buffer)
            .map_err(|error| http_error(400, error.to_string()))?;
        if count == 0 {
            return Err(http_error(400, "incomplete HTTP headers"));
        }
        bytes.extend_from_slice(&buffer[..count]);
    };
    if header_end > MAX_HEADERS {
        return Err(http_error(431, "header block exceeds 32 KiB"));
    }
    let header = std::str::from_utf8(&bytes[..header_end])
        .map_err(|_| http_error(400, "headers are not UTF-8"))?;
    let mut lines = header.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    if request_line.len() > MAX_REQUEST_LINE {
        return Err(http_error(414, "request line exceeds 8 KiB"));
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let version = parts.next().unwrap_or_default();
    if parts.next().is_some() || !version.starts_with("HTTP/1.") || !target.starts_with('/') {
        return Err(http_error(400, "malformed request line"));
    }
    if !matches!(method.as_str(), "GET" | "POST" | "PATCH" | "DELETE") {
        return Err(http_error(405, "method not allowed"));
    }
    let mut headers = BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| http_error(400, "malformed header"))?;
        let name = name.trim().to_ascii_lowercase();
        if name.is_empty()
            || headers
                .insert(name.clone(), value.trim().to_string())
                .is_some()
        {
            return Err(http_error(
                400,
                format!("duplicate or empty header: {name}"),
            ));
        }
    }
    if headers
        .get("transfer-encoding")
        .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
    {
        return Err(http_error(400, "chunked request bodies are unsupported"));
    }
    let content_length = headers
        .get("content-length")
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| http_error(400, "invalid Content-Length"))
        })
        .transpose()?
        .unwrap_or(0);
    if content_length > MAX_BODY {
        return Err(http_error(413, "request body exceeds 1 MiB"));
    }
    while bytes.len() < header_end + content_length {
        let mut buffer = [0_u8; 8192];
        let count = stream
            .read(&mut buffer)
            .map_err(|error| http_error(400, error.to_string()))?;
        if count == 0 {
            return Err(http_error(400, "incomplete request body"));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    let (raw_path, raw_query) = target.split_once('?').unwrap_or((&target, ""));
    let path = percent_decode(raw_path)?;
    if path.split('/').any(|segment| segment == "..") {
        return Err(http_error(400, "parent path segments are forbidden"));
    }
    let mut query = BTreeMap::new();
    for item in raw_query.split('&').filter(|item| !item.is_empty()) {
        let (name, value) = item.split_once('=').unwrap_or((item, ""));
        let name = percent_decode_query(name)?;
        let value = percent_decode_query(value)?;
        query.entry(name).or_insert_with(Vec::new).push(value);
    }
    Ok(Request {
        method,
        path,
        query,
        headers,
        body: bytes[header_end..header_end + content_length].to_vec(),
    })
}

fn authorize_request(
    request: &Request,
    security: &ServerSecurity,
) -> Result<Authentication, HttpError> {
    let host = request
        .headers
        .get("host")
        .ok_or_else(|| http_error(403, "missing or forbidden Host header"))?;
    let ip_host = format!("127.0.0.1:{}", security.port);
    let local_host = format!("localhost:{}", security.port);
    if !host.eq_ignore_ascii_case(&ip_host) && !host.eq_ignore_ascii_case(&local_host) {
        return Err(http_error(403, "missing or forbidden Host header"));
    }

    if let Some(origin) = request.headers.get("origin") {
        let allowed = format!("http://{host}");
        if !origin.eq_ignore_ascii_case(&allowed) {
            return Err(http_error(403, "cross-origin request forbidden"));
        }
    }

    if request.headers.get("sec-fetch-site").is_some_and(|site| {
        !site.eq_ignore_ascii_case("same-origin") && !site.eq_ignore_ascii_case("none")
    }) {
        return Err(http_error(403, "cross-site request forbidden"));
    }

    let (supplied_token, authentication) = if let Some(token) = request.headers.get("x-mote-token")
    {
        (token.as_str(), Authentication::Header)
    } else if let Some(token) = request
        .headers
        .get("cookie")
        .and_then(|header| cookie_value(header, CONSOLE_COOKIE))
    {
        (token, Authentication::Cookie)
    } else if request.method == "GET" && request.path == "/" {
        (
            request.query_first("t").unwrap_or_default(),
            Authentication::BootstrapQuery,
        )
    } else {
        ("", Authentication::Header)
    };
    if !constant_time_eq(supplied_token.as_bytes(), security.token.as_bytes()) {
        return Err(http_error(401, "missing or invalid launch token"));
    }

    if request.method != "GET" {
        let json = request
            .headers
            .get("content-type")
            .and_then(|value| value.split(';').next())
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"));
        if !json {
            return Err(http_error(
                400,
                "writes require Content-Type: application/json",
            ));
        }
    }
    Ok(authentication)
}

fn cookie_value<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').find_map(|item| {
        let (cookie_name, value) = item.trim().split_once('=')?;
        (cookie_name == name).then_some(value)
    })
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        difference |= usize::from(a ^ b);
    }
    difference == 0
}

fn create_launch_token(store: &Store) -> MoteResult<String> {
    let mut random = [0_u8; 16];
    rand::thread_rng().fill_bytes(&mut random);
    const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut token = String::with_capacity(random.len() * 2);
    for byte in random {
        token.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        token.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    let path = store.local_dir().join(TOKEN_FILE);

    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        if path.exists() {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(token.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }

    #[cfg(not(unix))]
    fs::write(path, format!("{token}\n"))?;

    Ok(token)
}

fn percent_decode(path: &str) -> Result<String, HttpError> {
    let input = path.as_bytes();
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index] == b'%' {
            if index + 2 >= input.len() {
                return Err(http_error(400, "invalid percent-encoding"));
            }
            let digits = std::str::from_utf8(&input[index + 1..index + 3])
                .map_err(|_| http_error(400, "invalid percent-encoding"))?;
            output.push(
                u8::from_str_radix(digits, 16)
                    .map_err(|_| http_error(400, "invalid percent-encoding"))?,
            );
            index += 3;
        } else {
            output.push(input[index]);
            index += 1;
        }
    }
    String::from_utf8(output).map_err(|_| http_error(400, "value is not UTF-8"))
}

fn percent_decode_query(value: &str) -> Result<String, HttpError> {
    percent_decode(&value.replace('+', " "))
}

fn http_error(status: u16, message: impl Into<String>) -> HttpError {
    HttpError {
        status,
        message: message.into(),
    }
}

fn write_response(stream: &mut TcpStream, status: u16, body: &[u8]) -> MoteResult<()> {
    write_typed_response(stream, status, "application/json", body)
}

fn write_typed_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> MoteResult<()> {
    write_typed_response_with_headers(stream, status, content_type, &[], body)
}

fn write_typed_response_with_headers(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> MoteResult<()> {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        414 => "URI Too Long",
        422 => "Unprocessable Entity",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n",
        body.len()
    )?;
    for (name, value) in headers {
        write!(stream, "{name}: {value}\r\n")?;
    }
    write!(stream, "Connection: close\r\n\r\n")?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn token_comparison_handles_equal_unequal_and_different_lengths() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secrex"));
        assert!(!constant_time_eq(b"secret", b"secret-longer"));
    }

    #[test]
    fn console_cookie_is_found_without_confusing_neighboring_names() {
        let header = "theme=dark; mote_console_token=secret; mote_console_token_old=stale";
        assert_eq!(cookie_value(header, CONSOLE_COOKIE), Some("secret"));
        assert_eq!(cookie_value(header, "missing"), None);
    }

    #[test]
    fn launch_token_is_128_random_bits_persisted_privately() {
        let temp = TempDir::new().unwrap();
        let store = Store::init(temp.path()).unwrap();
        let first = create_launch_token(&store).unwrap();
        let second = create_launch_token(&store).unwrap();

        assert_eq!(first.len(), 32);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, second);
        assert_eq!(
            fs::read_to_string(store.local_dir().join(TOKEN_FILE)).unwrap(),
            format!("{second}\n")
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.local_dir().join(TOKEN_FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn one_shared_snapshot_is_refreshed_after_publication() {
        use crate::ids::new_bead_id;
        use crate::op::{ScalarSet, make_create};
        use crate::publish;
        use jiff::Timestamp;

        let temp = TempDir::new().unwrap();
        let store = Store::init(temp.path()).unwrap();
        let context =
            ServerContext::new(store.clone(), ServerSecurity::new(7717, "token")).unwrap();
        assert!(context.snapshot().beads.is_empty());

        let id = new_bead_id();
        let set = ScalarSet {
            title: Some("watcher-visible".into()),
            ..Default::default()
        };
        publish::publish_op(
            &store,
            &make_create("tester".into(), id.clone(), set, Timestamp::now()),
        )
        .unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !context.snapshot().beads.contains_key(&id) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(context.snapshot().beads.contains_key(&id));

        let shared = context.clone();
        assert!(Arc::ptr_eq(&context.snapshot(), &shared.snapshot()));
    }
}
