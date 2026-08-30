use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use tempfile::TempDir;

use mote::repo::Store;
use mote::server::{ServerContext, ServerSecurity};

const TOKEN: &str = "00112233445566778899aabbccddeeff";

fn mote_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mote")
}

fn run_mote(temp: &TempDir, args: &[&str]) -> String {
    let output = Command::new(mote_bin())
        .args(args)
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "mote {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn exchange_raw(request: Vec<u8>) -> String {
    let temp = TempDir::new().unwrap();
    let store = Store::init(temp.path()).unwrap();
    exchange_on_store(&store, request)
}

fn exchange_on_store(store: &Store, request: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let store = store.clone();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let security = ServerSecurity::new(address.port(), TOKEN);
        let context = ServerContext::new(store, security).unwrap();
        mote::server::serve_connection(stream, &context).unwrap();
    });
    let request = String::from_utf8(request)
        .unwrap()
        .replace("{PORT}", &address.port().to_string());
    let mut client = TcpStream::connect(address).unwrap();
    client.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    server.join().unwrap();
    response
}

fn exchange(method: &str, path: &str, extra_headers: &str, body: &str) -> String {
    exchange_raw(
        format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{{PORT}}\r\nX-Mote-Token: {TOKEN}\r\n{extra_headers}Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes(),
    )
}

fn get_json(store: &Store, path: &str, actor: &str) -> serde_json::Value {
    let response = exchange_on_store(
        store,
        format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{{PORT}}\r\nX-Mote-Token: {TOKEN}\r\nX-Mote-Actor: {actor}\r\n\r\n"
        )
        .into_bytes(),
    );
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
    serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap()
}

fn write_json(
    store: &Store,
    method: &str,
    path: &str,
    actor: &str,
    body: serde_json::Value,
) -> (u16, Option<serde_json::Value>) {
    let body = serde_json::to_string(&body).unwrap();
    let response = exchange_on_store(
        store,
        format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{{PORT}}\r\nX-Mote-Token: {TOKEN}\r\nX-Mote-Actor: {actor}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes(),
    );
    let status = response
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse::<u16>()
        .unwrap();
    let body = response.split("\r\n\r\n").nth(1).unwrap();
    let json = (!body.is_empty()).then(|| serde_json::from_str(body).unwrap());
    (status, json)
}

fn read_until(stream: &mut TcpStream, marker: &[u8]) -> Vec<u8> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    while !bytes.windows(marker.len()).any(|window| window == marker) {
        let mut buffer = [0_u8; 4096];
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0, "stream closed before marker");
        bytes.extend_from_slice(&buffer[..count]);
    }
    bytes
}

fn exchange_with_process(port: u16, request: &str) -> String {
    let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    client.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    response
}

fn directory_snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn visit(root: &Path, path: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let mut entries: Vec<_> = fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }

    let mut snapshot = BTreeMap::new();
    visit(root, root, &mut snapshot);
    snapshot
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn occupied_console_port_fails_with_the_loopback_address_and_remedy() {
    let temp = TempDir::new().unwrap();
    Store::init(temp.path()).unwrap();
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = occupied.local_addr().unwrap().port().to_string();
    let output = Command::new(mote_bin())
        .args(["serve", "--port", &port])
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(&format!("cannot bind console to 127.0.0.1:{port}")));
    assert!(stderr.contains("Address already in use"), "{stderr}");
    assert!(stderr.contains("choose another --port"), "{stderr}");
}

#[test]
fn health_is_json_authenticated_and_every_response_closes_the_connection() {
    let response = exchange("GET", "/api/health", "", "");
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
    assert!(response.contains("Connection: close\r\n"));
    assert!(response.contains("Cache-Control: no-store\r\n"));
    assert!(response.contains("\"store_id\":\"st-"));
    assert!(!response.to_ascii_lowercase().contains("access-control-"));
}

#[test]
fn embedded_console_assets_are_authenticated_and_spa_paths_fall_back_to_live_index() {
    let bootstrap = exchange_raw(
        format!(
            "GET /?t={TOKEN} HTTP/1.1\r\nHost: 127.0.0.1:{{PORT}}\r\nSec-Fetch-Site: none\r\n\r\n"
        )
        .into_bytes(),
    );
    assert!(bootstrap.starts_with("HTTP/1.1 200 OK\r\n"), "{bootstrap}");
    assert!(bootstrap.contains(&format!(
        "Set-Cookie: mote_console_token={TOKEN}; HttpOnly; SameSite=Strict; Path=/\r\n"
    )));
    assert!(bootstrap.contains("window.__MOTE_LIVE__ = true"));
    assert!(bootstrap.contains("history.replaceState"));
    assert!(!bootstrap.contains(&format!("/console.js?t={TOKEN}")));
    assert!(!bootstrap.contains(&format!("/console.css?t={TOKEN}")));

    let index = exchange("GET", "/issues/bd-example", "", "");
    assert!(index.starts_with("HTTP/1.1 200 OK\r\n"), "{index}");
    assert!(index.contains("Content-Type: text/html; charset=utf-8\r\n"));
    assert!(index.contains("window.__MOTE_LIVE__ = true"));
    assert!(!index.contains("Set-Cookie:"));

    let script = exchange("GET", "/console.js", "", "");
    assert!(script.starts_with("HTTP/1.1 200 OK\r\n"), "{script}");
    assert!(script.contains("Content-Type: text/javascript; charset=utf-8\r\n"));
    assert_eq!(
        script.split("\r\n\r\n").nth(1).unwrap().as_bytes(),
        include_bytes!("../web/dist/console.js")
    );

    let style = exchange("GET", "/console.css", "", "");
    assert!(style.starts_with("HTTP/1.1 200 OK\r\n"), "{style}");
    assert!(style.contains("Content-Type: text/css; charset=utf-8\r\n"));
    assert_eq!(
        style.split("\r\n\r\n").nth(1).unwrap().as_bytes(),
        include_bytes!("../web/dist/console.css")
    );

    let cookie_authenticated = exchange_raw(
        format!("GET /console.js HTTP/1.1\r\nHost: 127.0.0.1:{{PORT}}\r\nCookie: mote_console_token={TOKEN}\r\n\r\n")
            .into_bytes(),
    );
    assert!(
        cookie_authenticated.starts_with("HTTP/1.1 200 OK\r\n"),
        "{cookie_authenticated}"
    );

    let query_authenticated = exchange_raw(
        format!("GET /console.js?t={TOKEN} HTTP/1.1\r\nHost: 127.0.0.1:{{PORT}}\r\n\r\n")
            .into_bytes(),
    );
    assert!(
        query_authenticated.starts_with("HTTP/1.1 401 Unauthorized\r\n"),
        "{query_authenticated}"
    );

    let unauthenticated =
        exchange_raw(b"GET /console.js HTTP/1.1\r\nHost: 127.0.0.1:{PORT}\r\n\r\n".to_vec());
    assert!(
        unauthenticated.starts_with("HTTP/1.1 401 Unauthorized\r\n"),
        "{unauthenticated}"
    );
}

#[test]
fn every_request_requires_the_launch_token() {
    let response =
        exchange_raw(b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1:{PORT}\r\n\r\n".to_vec());
    assert!(response.starts_with("HTTP/1.1 401 Unauthorized\r\n"));

    let response = exchange_raw(
        format!("GET /api/health?t={TOKEN} HTTP/1.1\r\nHost: 127.0.0.1:{{PORT}}\r\n\r\n")
            .into_bytes(),
    );
    assert!(response.starts_with("HTTP/1.1 401 Unauthorized\r\n"));

    let response = exchange_raw(
        format!("GET /api/health HTTP/1.1\r\nHost: 127.0.0.1:{{PORT}}\r\nCookie: mote_console_token={TOKEN}\r\n\r\n")
            .into_bytes(),
    );
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
}

#[test]
fn forged_host_and_cross_origin_requests_are_forbidden() {
    let response = exchange_raw(
        format!(
            "GET /api/health HTTP/1.1\r\nHost: attacker.example\r\nX-Mote-Token: {TOKEN}\r\n\r\n"
        )
        .into_bytes(),
    );
    assert!(response.starts_with("HTTP/1.1 403 Forbidden\r\n"));

    let response = exchange(
        "GET",
        "/api/health",
        "Origin: https://attacker.example\r\n",
        "",
    );
    assert!(response.starts_with("HTTP/1.1 403 Forbidden\r\n"));

    let response = exchange("GET", "/api/health", "Sec-Fetch-Site: cross-site\r\n", "");
    assert!(response.starts_with("HTTP/1.1 403 Forbidden\r\n"));
}

#[test]
fn writes_require_json_content_type() {
    let response = exchange("POST", "/api/not-yet-implemented", "", "{}");
    assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));

    let response = exchange(
        "POST",
        "/api/not-yet-implemented",
        "Content-Type: application/json; charset=utf-8\r\n",
        "{}",
    );
    assert!(response.starts_with("HTTP/1.1 404 Not Found\r\n"));
}

#[test]
fn validation_422_publishes_nothing_but_conflict_409_preserves_the_rejected_op() {
    let temp = TempDir::new().unwrap();
    let store = Store::init(temp.path()).unwrap();
    let headers = "X-Mote-Actor: console-user\r\nContent-Type: application/json\r\n";

    let invalid_body = r#"{"title":"","priority":9}"#;
    let invalid = exchange_on_store(
        &store,
        format!(
            "POST /api/beads HTTP/1.1\r\nHost: 127.0.0.1:{{PORT}}\r\nX-Mote-Token: {TOKEN}\r\n{headers}Content-Length: {}\r\n\r\n{invalid_body}",
            invalid_body.len()
        )
        .into_bytes(),
    );
    assert!(
        invalid.starts_with("HTTP/1.1 422 Unprocessable Entity\r\n"),
        "{invalid}"
    );
    assert!(store.list_op_filenames().unwrap().is_empty());

    let bad_ttl_body = r#"{"ttl":"tomorrow"}"#;
    let bad_ttl = exchange_on_store(
        &store,
        format!(
            "POST /api/beads/bd-missing/claim HTTP/1.1\r\nHost: 127.0.0.1:{{PORT}}\r\nX-Mote-Token: {TOKEN}\r\n{headers}Content-Length: {}\r\n\r\n{bad_ttl_body}",
            bad_ttl_body.len()
        )
        .into_bytes(),
    );
    assert!(
        bad_ttl.starts_with("HTTP/1.1 422 Unprocessable Entity\r\n"),
        "{bad_ttl}"
    );
    assert!(store.list_op_filenames().unwrap().is_empty());

    let create_body = r#"{"title":"original","priority":1}"#;
    let created = exchange_on_store(
        &store,
        format!(
            "POST /api/beads HTTP/1.1\r\nHost: 127.0.0.1:{{PORT}}\r\nX-Mote-Token: {TOKEN}\r\n{headers}Content-Length: {}\r\n\r\n{create_body}",
            create_body.len()
        )
        .into_bytes(),
    );
    assert!(created.starts_with("HTTP/1.1 201 Created\r\n"), "{created}");
    let created_body = created.split("\r\n\r\n").nth(1).unwrap();
    let bead_id = serde_json::from_str::<serde_json::Value>(created_body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(store.list_op_filenames().unwrap().len(), 1);

    let patch_body = r#"{"fields":{"title":"replacement"},"clock":{"title":"stale-clock"}}"#;
    let conflict = exchange_on_store(
        &store,
        format!(
            "PATCH /api/beads/{bead_id} HTTP/1.1\r\nHost: 127.0.0.1:{{PORT}}\r\nX-Mote-Token: {TOKEN}\r\n{headers}Content-Length: {}\r\n\r\n{patch_body}",
            patch_body.len()
        )
        .into_bytes(),
    );
    assert!(
        conflict.starts_with("HTTP/1.1 409 Conflict\r\n"),
        "{conflict}"
    );
    let conflict_body = conflict.split("\r\n\r\n").nth(1).unwrap();
    let conflict_json: serde_json::Value = serde_json::from_str(conflict_body).unwrap();
    assert!(conflict_json["op_id"].as_str().unwrap().contains("-p"));
    assert!(conflict_json["reason"].as_str().unwrap().contains("clock"));
    assert_eq!(conflict_json["current"]["title"], "original");
    assert_eq!(store.list_op_filenames().unwrap().len(), 2);
}

#[test]
fn parser_rejects_chunking_traversal_bad_methods_and_oversize_bodies() {
    let cases = [
        (
            format!(
                "POST /api/health HTTP/1.1\r\nHost: 127.0.0.1:{{PORT}}\r\nX-Mote-Token: {TOKEN}\r\nTransfer-Encoding: chunked\r\n\r\n"
            )
            .into_bytes(),
            "400 Bad Request",
        ),
        (
            b"GET /api/%2e%2e/secret HTTP/1.1\r\n\r\n".to_vec(),
            "400 Bad Request",
        ),
        (
            b"PUT /api/health HTTP/1.1\r\n\r\n".to_vec(),
            "405 Method Not Allowed",
        ),
        (
            b"POST /api/nope HTTP/1.1\r\nContent-Length: 1048577\r\n\r\n".to_vec(),
            "413 Payload Too Large",
        ),
    ];
    for (request, expected) in cases {
        let response = exchange_raw(request);
        assert!(
            response.contains(expected),
            "expected {expected}: {response}"
        );
    }
}

#[test]
fn unsupported_routes_are_honest_404s() {
    let response = exchange("GET", "/api/not-yet-implemented", "", "");
    assert!(response.starts_with("HTTP/1.1 404 Not Found\r\n"));
    assert!(response.contains("route not found"));
}

#[test]
fn read_routes_are_snapshot_backed_and_cli_shape_conformant() {
    let temp = TempDir::new().unwrap();
    let store = Store::init(temp.path()).unwrap();
    let bead_id = run_mote(
        &temp,
        &["new", "API bead", "--body", "details", "--actor", "alice"],
    );
    run_mote(
        &temp,
        &[
            "discuss",
            "topic",
            "new",
            "api",
            "--title",
            "API topic",
            "--body",
            "root body",
            "--actor",
            "alice",
        ],
    );
    let posts: serde_json::Value = serde_json::from_str(&run_mote(
        &temp,
        &["--json", "discuss", "list", "--topic", "api"],
    ))
    .unwrap();
    let root_post = posts[0]["post_id"].as_str().unwrap().to_string();
    run_mote(
        &temp,
        &[
            "discuss",
            "post",
            "--topic",
            "api",
            "--reply-to",
            &root_post,
            "--body",
            "child reply",
            "--actor",
            "bob",
        ],
    );
    run_mote(
        &temp,
        &["discuss", "needs-bead", &root_post, "--actor", "alice"],
    );
    run_mote(
        &temp,
        &[
            "msg",
            "send",
            "--to",
            "bob",
            "--kind",
            "request",
            "review this",
            "--actor",
            "alice",
        ],
    );
    let op_count = store.list_op_filenames().unwrap().len();

    let board = get_json(&store, "/api/board", "alice");
    let board_cli: serde_json::Value =
        serde_json::from_str(&run_mote(&temp, &["--json", "board", "--actor", "alice"])).unwrap();
    assert_eq!(board["status_counts"], board_cli["status_counts"]);
    assert_eq!(board["active_claims"], board_cli["active_claims"]);
    assert_eq!(board["actor"], "alice");

    let beads = get_json(&store, "/api/beads?all=1", "alice");
    let beads_cli: serde_json::Value = serde_json::from_str(&run_mote(
        &temp,
        &["--json", "ls", "--all", "--actor", "alice"],
    ))
    .unwrap();
    assert_eq!(beads, beads_cli);

    let detail = get_json(&store, &format!("/api/beads/{bead_id}"), "alice");
    let detail_cli: serde_json::Value =
        serde_json::from_str(&run_mote(&temp, &["--json", "show", &bead_id])).unwrap();
    assert_eq!(detail, detail_cli);

    let history = get_json(&store, &format!("/api/beads/{bead_id}/history"), "alice");
    let history_cli: serde_json::Value = serde_json::from_str(&run_mote(
        &temp,
        &["--json", "history", &bead_id, "--include-rejected"],
    ))
    .unwrap();
    assert_eq!(history, history_cli);

    let topics = get_json(&store, "/api/topics", "alice");
    let topics_cli: serde_json::Value =
        serde_json::from_str(&run_mote(&temp, &["--json", "discuss", "topics"])).unwrap();
    assert_eq!(topics, topics_cli);

    let posts = get_json(&store, "/api/topics/api/posts", "alice");
    let posts_cli: serde_json::Value = serde_json::from_str(&run_mote(
        &temp,
        &["--json", "discuss", "list", "--topic", "api"],
    ))
    .unwrap();
    assert_eq!(posts, posts_cli);

    let unread = get_json(&store, "/api/unread", "alice");
    let unread_cli: serde_json::Value = serde_json::from_str(&run_mote(
        &temp,
        &["--json", "discuss", "unread", "--actor", "alice"],
    ))
    .unwrap();
    assert_eq!(unread, unread_cli);

    let thread = get_json(&store, &format!("/api/posts/{root_post}/thread"), "alice");
    let thread_cli: serde_json::Value = serde_json::from_str(&run_mote(
        &temp,
        &["--json", "discuss", "thread", &root_post],
    ))
    .unwrap();
    assert_eq!(thread, thread_cli);

    let unrouted = get_json(&store, "/api/unrouted", "alice");
    let unrouted_cli: serde_json::Value =
        serde_json::from_str(&run_mote(&temp, &["--json", "discuss", "unrouted"])).unwrap();
    assert_eq!(unrouted, unrouted_cli);

    let search = get_json(&store, "/api/search?q=root&topic=api", "alice");
    let search_cli: serde_json::Value = serde_json::from_str(&run_mote(
        &temp,
        &["--json", "discuss", "search", "root", "--topic", "api"],
    ))
    .unwrap();
    assert_eq!(search, search_cli);

    let actors = get_json(&store, "/api/actors", "alice");
    let actor_names: Vec<_> = actors
        .as_array()
        .unwrap()
        .iter()
        .map(|actor| actor["actor"].clone())
        .collect();
    let actors_cli: serde_json::Value = serde_json::from_str(&run_mote(
        &temp,
        &["--json", "actor", "list", "--actor", "alice"],
    ))
    .unwrap();
    let actor_names_cli: Vec<_> = actors_cli
        .as_array()
        .unwrap()
        .iter()
        .map(|actor| actor["actor"].clone())
        .collect();
    assert_eq!(actor_names, actor_names_cli);
    assert!(
        actors
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row.as_object().unwrap().contains_key("last_message"))
    );

    let dm = get_json(&store, "/api/dm/bob", "alice");
    let dm_cli: serde_json::Value = serde_json::from_str(&run_mote(
        &temp,
        &["--json", "msg", "thread", "bob", "--actor", "alice"],
    ))
    .unwrap();
    assert_eq!(dm, dm_cli);

    let inflight = get_json(&store, "/api/inflight?minutes=60", "alice");
    let inflight_cli: serde_json::Value = serde_json::from_str(&run_mote(
        &temp,
        &[
            "--json",
            "in-flight",
            "--minutes",
            "60",
            "--no-git",
            "--actor",
            "alice",
        ],
    ))
    .unwrap();
    let keys: std::collections::BTreeSet<_> = inflight.as_object().unwrap().keys().collect();
    let cli_keys: std::collections::BTreeSet<_> =
        inflight_cli.as_object().unwrap().keys().collect();
    assert_eq!(keys, cli_keys);
    assert_eq!(inflight["actor"], inflight_cli["actor"]);
    assert_eq!(inflight["window_minutes"], 60);

    assert_eq!(
        store.list_op_filenames().unwrap().len(),
        op_count,
        "GET routes must remain read-only"
    );
}

#[test]
fn every_named_write_route_publishes_existing_ops_under_the_header_actor() {
    let temp = TempDir::new().unwrap();
    let store = Store::init(temp.path()).unwrap();
    assert!(!store.local_dir().join("actor").exists());

    let (status, created) = write_json(
        &store,
        "POST",
        "/api/beads",
        "web-alice",
        serde_json::json!({"title": "write target", "priority": 1}),
    );
    assert_eq!(status, 201);
    let bead_id = created.unwrap()["id"].as_str().unwrap().to_string();

    assert_eq!(
        write_json(
            &store,
            "POST",
            &format!("/api/beads/{bead_id}/notes"),
            "web-alice",
            serde_json::json!({"kind": "progress", "text": "via console"}),
        )
        .0,
        204
    );
    assert_eq!(
        write_json(
            &store,
            "POST",
            &format!("/api/beads/{bead_id}/tags"),
            "web-alice",
            serde_json::json!({"tags": ["console", "api"], "add": true}),
        )
        .0,
        204
    );

    let (_, parent) = write_json(
        &store,
        "POST",
        "/api/beads",
        "web-alice",
        serde_json::json!({"title": "dependency"}),
    );
    let parent_id = parent.unwrap()["id"].as_str().unwrap().to_string();
    assert_eq!(
        write_json(
            &store,
            "POST",
            &format!("/api/beads/{bead_id}/deps"),
            "web-alice",
            serde_json::json!({"parent": parent_id, "kind": "blocks", "add": true}),
        )
        .0,
        204
    );
    assert_eq!(
        write_json(
            &store,
            "POST",
            &format!("/api/beads/{bead_id}/claim"),
            "web-alice",
            serde_json::json!({"ttl": 300}),
        )
        .0,
        204
    );
    assert_eq!(
        write_json(
            &store,
            "POST",
            &format!("/api/beads/{bead_id}/release"),
            "web-alice",
            serde_json::json!({}),
        )
        .0,
        204
    );

    let (status, reservation) = write_json(
        &store,
        "POST",
        "/api/reservations",
        "web-alice",
        serde_json::json!({"issue": bead_id, "paths": ["src/console.rs"], "ttl": 300}),
    );
    assert_eq!(status, 201);
    let reservation_id = reservation.unwrap()["reservation_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        write_json(
            &store,
            "DELETE",
            &format!("/api/reservations/{reservation_id}"),
            "web-alice",
            serde_json::json!({}),
        )
        .0,
        204
    );

    assert_eq!(
        write_json(
            &store,
            "POST",
            "/api/topics",
            "web-alice",
            serde_json::json!({"topic": "console-api", "title": "Console API", "body": "first post"}),
        )
        .0,
        201
    );
    let posts = get_json(&store, "/api/topics/console-api/posts", "web-alice");
    let root_post = posts[0]["post_id"].as_str().unwrap().to_string();
    let (status, reply) = write_json(
        &store,
        "POST",
        "/api/topics/console-api/posts",
        "web-bob",
        serde_json::json!({"body": "reply", "reply_to": root_post}),
    );
    assert_eq!(status, 201);
    let reply_id = reply.unwrap()["post_id"].as_str().unwrap().to_string();
    assert_eq!(
        write_json(
            &store,
            "POST",
            &format!("/api/posts/{root_post}/sticky"),
            "web-alice",
            serde_json::json!({"sticky": true}),
        )
        .0,
        204
    );
    let (status, promoted) = write_json(
        &store,
        "POST",
        &format!("/api/posts/{root_post}/promote"),
        "web-alice",
        serde_json::json!({"title": "promoted work", "body": "from post", "priority": 1, "tags": ["console"]}),
    );
    assert_eq!(status, 201);
    assert!(promoted.unwrap()["id"].as_str().is_some());
    assert_eq!(
        write_json(
            &store,
            "POST",
            &format!("/api/posts/{reply_id}/route"),
            "web-bob",
            serde_json::json!({"issue": bead_id}),
        )
        .0,
        204
    );
    assert_eq!(
        write_json(
            &store,
            "POST",
            &format!("/api/posts/{reply_id}/needs-bead"),
            "web-bob",
            serde_json::json!({}),
        )
        .0,
        204
    );
    assert_eq!(
        write_json(
            &store,
            "POST",
            &format!("/api/posts/{reply_id}/resolve"),
            "web-bob",
            serde_json::json!({}),
        )
        .0,
        204
    );
    assert_eq!(
        write_json(
            &store,
            "POST",
            "/api/discussion/read",
            "web-alice",
            serde_json::json!({"topic": "console-api", "through": reply_id}),
        )
        .0,
        204
    );

    let (status, message) = write_json(
        &store,
        "POST",
        "/api/messages",
        "web-alice",
        serde_json::json!({
            "to": "web-bob", "body": "please review", "kind": "request",
            "entity": bead_id, "idempotency_key": "console-request-1"
        }),
    );
    assert_eq!(status, 201);
    let message = message.unwrap();
    assert_eq!(message["accepted"], true);
    assert_eq!(message["delivery"], "queued");
    assert_eq!(message["recipient"], "web-bob");
    assert_eq!(message["recipient_presence"]["state"], "recent");
    assert_eq!(message["recipient_presence"]["source"], "accepted_activity");
    assert_eq!(
        message["recipient_presence"]["reason"],
        "sessionless_recent_activity"
    );
    assert_eq!(message["idempotent_replay"], false);
    let message_id = message["msg_id"].as_str().unwrap().to_string();
    let before_retry = store.list_op_filenames().unwrap().len();
    let (retry_status, retry) = write_json(
        &store,
        "POST",
        "/api/messages",
        "web-alice",
        serde_json::json!({
            "to": "web-bob", "body": "please review", "kind": "request",
            "entity": bead_id, "idempotency_key": "console-request-1"
        }),
    );
    assert_eq!(retry_status, 200);
    let retry = retry.unwrap();
    assert_eq!(retry["msg_id"], message_id);
    assert_eq!(retry["recipient_presence"], message["recipient_presence"]);
    assert_eq!(retry["idempotent_replay"], true);
    assert_eq!(store.list_op_filenames().unwrap().len(), before_retry);
    assert_eq!(
        write_json(
            &store,
            "POST",
            &format!("/api/messages/{message_id}/ack"),
            "web-bob",
            serde_json::json!({}),
        )
        .0,
        204
    );
    let (reply_status, reply) = write_json(
        &store,
        "POST",
        &format!("/api/messages/{message_id}/reply"),
        "web-bob",
        serde_json::json!({"body": "done", "kind": "response", "idempotency_key": "console-reply-1"}),
    );
    assert_eq!(reply_status, 201);
    let reply_message_id = reply.unwrap()["msg_id"].clone();
    let before_reply_retry = store.list_op_filenames().unwrap().len();
    let (reply_retry_status, reply_retry) = write_json(
        &store,
        "POST",
        &format!("/api/messages/{message_id}/reply"),
        "web-bob",
        serde_json::json!({"body": "done", "kind": "response", "idempotency_key": "console-reply-1"}),
    );
    assert_eq!(reply_retry_status, 200);
    assert_eq!(reply_retry.unwrap()["msg_id"], reply_message_id);
    assert_eq!(store.list_op_filenames().unwrap().len(), before_reply_retry);
    assert_eq!(
        write_json(
            &store,
            "POST",
            &format!("/api/messages/{message_id}/resolve"),
            "web-alice",
            serde_json::json!({}),
        )
        .0,
        204
    );

    assert_eq!(
        write_json(
            &store,
            "POST",
            &format!("/api/beads/{bead_id}/close"),
            "web-alice",
            serde_json::json!({"note": "closed from console"}),
        )
        .0,
        204
    );

    let state = mote::reducer::replay_store(&store).unwrap();
    assert_eq!(state.beads[&bead_id].status.as_str(), "closed");
    assert!(state.beads[&bead_id].tags.contains("console"));
    assert!(
        state.beads[&bead_id]
            .deps
            .contains(&(parent_id, "blocks".into()))
    );
    assert_eq!(
        state.messages[&message_id].request_state.unwrap().as_str(),
        "resolved"
    );
    assert!(
        state
            .history
            .values()
            .flatten()
            .filter(|entry| entry.accepted)
            .all(|entry| entry.actor.starts_with("web-"))
    );
    assert!(
        !store.local_dir().join("actor").exists(),
        "the server must never mutate the checkout actor file"
    );
}

#[test]
fn sse_streams_verbatim_events_with_resume_ids_from_the_single_hub() {
    use jiff::Timestamp;
    use mote::op::{ScalarSet, make_create};

    let temp = TempDir::new().unwrap();
    let store = Store::init(temp.path()).unwrap();
    let first_id = mote::ids::new_bead_id();
    let first = mote::publish::publish_op(
        &store,
        &make_create(
            "cli-peer".into(),
            first_id,
            ScalarSet {
                title: Some("before cursor".into()),
                ..Default::default()
            },
            Timestamp::now(),
        ),
    )
    .unwrap();
    let second_id = mote::ids::new_bead_id();
    let second = mote::publish::publish_op(
        &store,
        &make_create(
            "cli-peer".into(),
            second_id,
            ScalarSet {
                title: Some("resume me".into()),
                ..Default::default()
            },
            Timestamp::now(),
        ),
    )
    .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let context =
        ServerContext::new(store.clone(), ServerSecurity::new(address.port(), TOKEN)).unwrap();
    let server_context = context.clone();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        match mote::server::serve_connection(stream, &server_context) {
            Ok(()) => {}
            Err(mote::errors::MoteError::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                ) => {}
            Err(error) => panic!("SSE connection failed: {error}"),
        }
    });
    let mut client = TcpStream::connect(address).unwrap();
    client
        .write_all(
            format!(
                "GET /api/events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: mote_console_token={TOKEN}\r\nLast-Event-ID: {}\r\n\r\n",
                address.port(),
                first.as_str(),
            )
            .as_bytes(),
        )
        .unwrap();
    let bytes = read_until(&mut client, b"\n\n");
    let response = String::from_utf8(bytes).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
    assert!(response.contains("Content-Type: text/event-stream\r\n"));
    assert!(response.contains("Cache-Control: no-store\r\n"));
    assert!(response.contains("X-Accel-Buffering: no\r\n"));
    let frame = response.split("\r\n\r\n").nth(1).unwrap();
    let id = frame
        .lines()
        .find_map(|line| line.strip_prefix("id: "))
        .unwrap();
    let data = frame
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .unwrap();
    let envelope: serde_json::Value = serde_json::from_str(data).unwrap();
    assert_eq!(id, envelope["event_id"]);
    assert_eq!(id, second.as_str());
    assert_eq!(envelope["schema"], "mote.event.v1");
    assert_eq!(envelope["actor"], "cli-peer");

    let live = mote::publish::publish_op(
        &store,
        &make_create(
            "cli-peer".into(),
            mote::ids::new_bead_id(),
            ScalarSet {
                title: Some("stream live".into()),
                ..Default::default()
            },
            Timestamp::now(),
        ),
    )
    .unwrap();
    let live_bytes = read_until(&mut client, b"\n\n");
    let live_frame = String::from_utf8(live_bytes).unwrap();
    let live_id = live_frame
        .lines()
        .find_map(|line| line.strip_prefix("id: "))
        .unwrap();
    let live_data = live_frame
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .unwrap();
    let live_envelope: serde_json::Value = serde_json::from_str(live_data).unwrap();
    assert_eq!(live_id, live.as_str());
    assert_eq!(live_envelope["event_id"], live.as_str());
    assert_eq!(live_envelope["actor"], "cli-peer");

    drop(client);
    mote::publish::publish_op(
        &store,
        &make_create(
            "cli-peer".into(),
            mote::ids::new_bead_id(),
            ScalarSet {
                title: Some("disconnect stream".into()),
                ..Default::default()
            },
            Timestamp::now(),
        ),
    )
    .unwrap();
    server.join().unwrap();
}

#[test]
fn real_serve_process_preserves_protocol_and_store_boundaries() {
    let temp = TempDir::new().unwrap();
    let store = Store::init(temp.path()).unwrap();
    let initial_tree = directory_snapshot(store.root());
    let initial_ops = store.list_op_filenames().unwrap().len();

    let child = Command::new(mote_bin())
        .args(["serve", "--port", "0"])
        .current_dir(temp.path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut server = ChildGuard(child);
    let mut launch = String::new();
    BufReader::new(server.0.stderr.take().unwrap())
        .read_line(&mut launch)
        .unwrap();
    let launch_url = launch
        .split_whitespace()
        .find(|part| part.starts_with("http://127.0.0.1:"))
        .unwrap_or_else(|| panic!("missing launch URL in {launch:?}"));
    let authority = launch_url
        .strip_prefix("http://127.0.0.1:")
        .unwrap()
        .split('/')
        .next()
        .unwrap();
    let port = authority.parse::<u16>().unwrap();
    assert_ne!(port, 0);
    let token_path = store.local_dir().join("serve-token");
    let token = fs::read_to_string(&token_path)
        .unwrap_or_else(|_| panic!("mote serve did not create {}", token_path.display()))
        .trim()
        .to_string();
    assert!(launch_url.ends_with(&format!("/?t={token}")));

    let missing = exchange_with_process(
        port,
        &format!("GET /api/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
    );
    assert!(
        missing.starts_with("HTTP/1.1 401 Unauthorized\r\n"),
        "{missing}"
    );
    let forged = exchange_with_process(
        port,
        &format!(
            "GET /api/health HTTP/1.1\r\nHost: attacker.invalid\r\nX-Mote-Token: {token}\r\n\r\n"
        ),
    );
    assert!(forged.starts_with("HTTP/1.1 403 Forbidden\r\n"), "{forged}");

    let json_request = |method: &str, path: &str, body: &serde_json::Value| {
        let body = serde_json::to_string(body).unwrap();
        exchange_with_process(
            port,
            &format!(
                "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Mote-Token: {token}\r\nX-Mote-Actor: web-operator\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
    };
    let create = json_request(
        "POST",
        "/api/beads",
        &serde_json::json!({"title": "created over real socket"}),
    );
    assert!(create.starts_with("HTTP/1.1 201 Created\r\n"), "{create}");
    let bead_id =
        serde_json::from_str::<serde_json::Value>(create.split("\r\n\r\n").nth(1).unwrap())
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
    assert_eq!(store.list_op_filenames().unwrap().len(), initial_ops + 1);

    let detail = exchange_with_process(
        port,
        &format!(
            "GET /api/beads/{bead_id} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Mote-Token: {token}\r\nX-Mote-Actor: web-operator\r\n\r\n"
        ),
    );
    assert!(detail.starts_with("HTTP/1.1 200 OK\r\n"), "{detail}");
    let detail: serde_json::Value =
        serde_json::from_str(detail.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    let original_title_clock = detail["clock"]["title"].as_str().unwrap().to_string();
    let accepted_patch = json_request(
        "PATCH",
        &format!("/api/beads/{bead_id}"),
        &serde_json::json!({
            "fields": {"title": "winning title"},
            "clock": {"title": original_title_clock},
        }),
    );
    assert!(
        accepted_patch.starts_with("HTTP/1.1 204 No Content\r\n"),
        "{accepted_patch}"
    );
    let stale_patch = json_request(
        "PATCH",
        &format!("/api/beads/{bead_id}"),
        &serde_json::json!({
            "fields": {"title": "stale title"},
            "clock": {"title": detail["clock"]["title"]},
        }),
    );
    assert!(
        stale_patch.starts_with("HTTP/1.1 409 Conflict\r\n"),
        "{stale_patch}"
    );
    let conflict: serde_json::Value =
        serde_json::from_str(stale_patch.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    let replayed = mote::reducer::replay_store(&store).unwrap();
    assert_eq!(
        replayed
            .rejection_reason(conflict["op_id"].as_str().unwrap())
            .as_deref(),
        conflict["reason"].as_str()
    );
    assert_eq!(conflict["current"]["title"], "winning title");

    let before_invalid = store.list_op_filenames().unwrap();
    let invalid_ttl = json_request(
        "POST",
        &format!("/api/beads/{bead_id}/claim"),
        &serde_json::json!({"ttl": 0}),
    );
    assert!(
        invalid_ttl.starts_with("HTTP/1.1 422 Unprocessable Entity\r\n"),
        "{invalid_ttl}"
    );
    assert_eq!(store.list_op_filenames().unwrap(), before_invalid);

    let mut events = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    events
        .write_all(
            format!("GET /api/events HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: mote_console_token={token}\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
    let headers = String::from_utf8(read_until(&mut events, b"\r\n\r\n")).unwrap();
    assert!(headers.starts_with("HTTP/1.1 200 OK\r\n"), "{headers}");

    let cli = Command::new(mote_bin())
        .args(["--actor", "cli-peer", "new", "external CLI event"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(
        cli.status.success(),
        "external CLI failed: {}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let external_id = String::from_utf8(cli.stdout).unwrap().trim().to_string();
    let event = (0..16)
        .find_map(|_| {
            let frame = String::from_utf8(read_until(&mut events, b"\n\n")).unwrap();
            frame
                .lines()
                .filter_map(|line| line.strip_prefix("data: "))
                .filter_map(|data| serde_json::from_str::<serde_json::Value>(data).ok())
                .find(|event| {
                    event["type"] == "issue.created"
                        && event["actor"] == "cli-peer"
                        && event["data"]["entity"] == external_id
                })
        })
        .expect("connected SSE stream did not receive the external CLI create");
    assert_eq!(event["type"], "issue.created");
    assert_eq!(event["actor"], "cli-peer");
    assert_eq!(event["data"]["entity"], external_id);
    drop(events);

    assert_eq!(store.list_op_filenames().unwrap().len(), initial_ops + 4);
    let before_kill = directory_snapshot(store.root());
    let changed: Vec<_> = before_kill
        .iter()
        .filter(|(path, bytes)| initial_tree.get(*path) != Some(*bytes))
        .map(|(path, _)| path.as_str())
        .collect();
    assert_eq!(
        changed
            .iter()
            .filter(|path| path.starts_with("ops/") && path.ends_with(".json"))
            .count(),
        4
    );
    assert!(changed.contains(&"local/serve-token"));
    assert!(changed.iter().all(|path| *path == "local/serve-token"
        || (path.starts_with("ops/") && path.ends_with(".json"))));

    server.0.kill().unwrap();
    server.0.wait().unwrap();
    assert_eq!(directory_snapshot(store.root()), before_kill);
}
