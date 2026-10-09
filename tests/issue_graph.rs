use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

fn run(dir: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mote"))
        .current_dir(dir.path())
        .env_remove("MOTE_STORE")
        .env_remove("MOTE_ACTOR")
        .args(args)
        .output()
        .unwrap()
}

fn ok(dir: &TempDir, args: &[&str]) -> String {
    let output = run(dir, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn setup() -> TempDir {
    let dir = TempDir::new().unwrap();
    ok(&dir, &["init"]);
    dir
}

fn issue(dir: &TempDir, id: &str, title: &str) {
    ok(dir, &["--actor", "alice", "new", title, "--id", id]);
}

fn mutate(dir: &TempDir, args: &[&str]) {
    let mut full = vec!["--actor", "alice"];
    full.extend_from_slice(args);
    ok(dir, &full);
}

fn graph(dir: &TempDir, args: &[&str]) -> Value {
    let mut full = vec!["--actor", "alice", "--json", "graph"];
    full.extend_from_slice(args);
    serde_json::from_str(&ok(dir, &full)).unwrap()
}

fn node<'a>(graph: &'a Value, id: &str) -> &'a Value {
    graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == id)
        .unwrap()
}

#[test]
fn diamond_preserves_every_edge_and_expands_shared_nodes_once() {
    let dir = setup();
    for (id, title) in [
        ("a", "Design"),
        ("b", "API"),
        ("c", "Client"),
        ("d", "Ship"),
    ] {
        issue(&dir, id, title);
    }
    for (child, parent) in [("b", "a"), ("c", "a"), ("d", "b"), ("d", "c")] {
        mutate(&dir, &["dep", "add", child, parent]);
    }
    let data = graph(&dir, &[]);
    assert_eq!(data["nodes"].as_array().unwrap().len(), 4);
    assert_eq!(data["edges"].as_array().unwrap().len(), 4);
    assert_eq!(node(&data, "a")["ready"], true);
    assert_eq!(node(&data, "d")["waiting"], true);
    let text = ok(&dir, &["graph"]);
    assert!(text.contains("├── → ◌ API"), "{text}");
    assert!(text.contains("└── → ↩ d  Ship"), "{text}");
    assert_eq!(text.matches("◌ Ship").count(), 1);
    assert!(!text.contains('\x1b'));
    assert_eq!(text, ok(&dir, &["ls", "--graph"]));
    let mermaid = ok(&dir, &["graph", "--format", "mermaid"]);
    assert_eq!(mermaid.matches(" -->|").count(), 4);
    assert!(mermaid.contains("n0 -->|\"blocks\"| n1"));
    assert!(mermaid.contains("n2 -->|\"blocks\"| n3"));
}

#[test]
fn hierarchy_is_nonblocking_even_when_it_forms_a_cycle_with_dependencies() {
    let dir = setup();
    issue(&dir, "epic", "Release");
    issue(&dir, "task", "Implement");
    mutate(&dir, &["rel", "add", "task", "epic"]);
    mutate(&dir, &["dep", "add", "epic", "task", "--kind", "parent"]);
    let data = graph(&dir, &[]);
    assert_eq!(node(&data, "task")["ready"], true);
    assert_eq!(node(&data, "epic")["ready"], false);
    assert_eq!(data["edges"].as_array().unwrap().len(), 2);
    let text = ok(&dir, &["graph"]);
    assert!(text.contains("┄ (parent)"), "{text}");
    assert!(text.contains("→ (parent) ↩ epic"), "{text}");
    let mermaid = ok(&dir, &["graph", "--format", "mermaid"]);
    assert!(mermaid.contains("-.->|\"relation: parent\"|"));
    assert!(mermaid.contains("-->|\"blocks: parent\"|"));
}

#[test]
fn closed_deleted_and_missing_prerequisites_are_satisfied_context() {
    let dir = setup();
    for id in ["closed", "deleted", "task", "unrelated"] {
        issue(&dir, id, id);
    }
    for parent in ["closed", "deleted"] {
        mutate(&dir, &["dep", "add", "task", parent]);
    }
    mutate(&dir, &["close", "closed"]);
    mutate(&dir, &["delete", "deleted"]);
    mutate(&dir, &["close", "unrelated"]);
    let data = graph(&dir, &[]);
    assert_eq!(data["nodes"].as_array().unwrap().len(), 3);
    assert_eq!(node(&data, "closed")["context"], true);
    assert_eq!(node(&data, "deleted")["status"], "deleted");
    assert_eq!(node(&data, "task")["ready"], true);
    assert!(
        data["edges"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["satisfied"] == true)
    );
    assert_eq!(
        graph(&dir, &["--all"])["nodes"].as_array().unwrap().len(),
        4
    );

    // Legacy or partial state can reference an absent parent. Keep the edge
    // visible, matching State::deps_satisfied instead of inventing a blocker.
    let store = mote::Store::open(&dir.path().join(".mote")).unwrap();
    let mut state = mote::reducer::replay_store(&store).unwrap();
    state
        .beads
        .get_mut("task")
        .unwrap()
        .deps
        .insert(("missing".into(), "blocks".into()));
    let projected = mote::graph::IssueGraph::build(
        &state,
        &["task".into()].into(),
        false,
        "alice",
        "2099-01-01T00:00:00Z",
    );
    let missing = projected.nodes.iter().find(|n| n.id == "missing").unwrap();
    assert_eq!(missing.status, "missing");
    assert!(!missing.ready);
    assert!(
        projected
            .edges
            .iter()
            .find(|e| e.from == "missing")
            .unwrap()
            .satisfied
    );
}

#[test]
fn filters_keep_ancestors_as_context_without_selecting_siblings() {
    let dir = setup();
    for id in ["parent", "prereq", "task", "sibling", "other"] {
        issue(&dir, id, id);
    }
    mutate(&dir, &["rel", "add", "task", "parent"]);
    mutate(&dir, &["rel", "add", "sibling", "parent"]);
    mutate(&dir, &["dep", "add", "task", "prereq"]);
    mutate(&dir, &["tag", "add", "task", "frontend"]);
    let text = ok(&dir, &["ls", "--tag", "frontend", "--graph"]);
    assert!(text.contains("1 selected · 2 context · 2 links"), "{text}");
    assert!(!text.contains("sibling"));
    assert!(!text.contains("other"));
    assert!(text.contains("[context]"));
    let focused = graph(&dir, &["task"]);
    assert_eq!(focused["nodes"].as_array().unwrap().len(), 4);
    assert!(!ok(&dir, &["graph", "task"]).contains("other"));
}

#[test]
fn ready_graph_shows_downstream_and_coblockers_and_respects_actor_claims() {
    let dir = setup();
    for id in ["ready", "claimed", "ship"] {
        issue(&dir, id, id);
    }
    for parent in ["ready", "claimed"] {
        mutate(&dir, &["dep", "add", "ship", parent]);
    }
    ok(&dir, &["--actor", "bob", "claim", "claimed"]);
    let data = graph(&dir, &["--ready"]);
    assert_eq!(node(&data, "ready")["context"], false);
    assert_eq!(node(&data, "ship")["context"], true);
    assert_eq!(node(&data, "claimed")["context"], true);
    assert_eq!(node(&data, "claimed")["ready"], false);
    assert_eq!(node(&data, "claimed")["claimed_by"], "bob");
    let text = ok(&dir, &["--actor", "alice", "ready", "--graph"]);
    assert!(text.contains("1 selected · 2 context · 2 links"), "{text}");
    assert_eq!(
        text,
        ok(&dir, &["--actor", "alice", "ls", "--ready", "--graph"])
    );
    assert_eq!(text, ok(&dir, &["--actor", "alice", "graph", "--ready"]));
    ok(&dir, &["--actor", "bob", "set", "claimed", "status=doing"]);
    ok(&dir, &["--actor", "bob", "release", "claimed"]);
    let data = graph(&dir, &["--ready"]);
    assert_eq!(node(&data, "claimed")["stranded"], true);
    assert_eq!(node(&data, "claimed")["ready"], true);
}

#[test]
fn empty_errors_json_and_untrusted_labels() {
    let dir = setup();
    assert_eq!(ok(&dir, &["graph"]), "No matching issues.\n");
    assert_eq!(
        graph(&dir, &[]),
        serde_json::json!({"nodes": [], "edges": []})
    );
    let absent = run(&dir, &["graph", "absent"]);
    assert_eq!(absent.status.code(), Some(3));
    for args in [
        vec!["ls", "--graph", "--json"],
        vec!["ready", "--graph", "--json"],
        vec!["graph", "--format", "mermaid", "--json"],
    ] {
        assert!(!run(&dir, &args).status.success());
    }

    issue(
        &dir,
        "safe",
        "A \"quote\" <script> & #35; | 日本語\n\x1b[31m",
    );
    let text = ok(&dir, &["graph"]);
    assert!(!text.contains('\x1b'));
    assert!(!text.contains("日本語\n"));
    let mermaid = ok(&dir, &["graph", "--format", "mermaid"]);
    assert!(
        mermaid.contains("#34;quote#34; #60;script#62; #38; #35;35#59; #124; 日本語"),
        "{mermaid}"
    );
    assert!(!mermaid.contains("<script>"));
    assert!(!mermaid.contains('\x1b'));
    // Existing list JSON stays an array; graph JSON is a separate contract.
    let list: Value = serde_json::from_str(&ok(&dir, &["ls", "--json"])).unwrap();
    assert!(list.is_array());
}

#[test]
fn long_chains_stay_narrow_without_losing_issues_or_edges() {
    let dir = setup();
    for i in 0..12 {
        let id = format!("task-{i:02}");
        issue(&dir, &id, &format!("Step {i}"));
        if i > 0 {
            mutate(&dir, &["dep", "add", &id, &format!("task-{:02}", i - 1)]);
        }
    }
    let text = ok(&dir, &["graph"]);
    assert!(text.contains("↪ task-04"), "{text}");
    assert!(text.contains("↪ task-08"), "{text}");
    for i in 0..12 {
        assert_eq!(text.matches(&format!("Step {i}  task-{i:02} ·")).count(), 1);
    }
    assert_eq!(
        text.lines().filter(|line| line.contains("└── →")).count(),
        11
    );
    assert!(
        text.lines()
            .all(|line| !line.starts_with("                "))
    );
}
