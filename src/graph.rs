//! Read-only issue graph projection and dependency-free renderers.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use serde::Serialize;

use crate::op::Status;
use crate::state::State;

#[derive(Debug, Serialize)]
pub struct IssueGraph {
    pub nodes: Vec<GraphNode>,
    /// Edges point from prerequisite/parent to dependent/child.
    pub edges: Vec<GraphEdge>,
}

#[derive(Debug, Serialize)]
pub struct GraphNode {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: i32,
    pub ready: bool,
    pub waiting: bool,
    pub stranded: bool,
    pub claimed_by: Option<String>,
    pub context: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub kind: String,
    pub blocking: bool,
    /// Closed/deleted/missing prerequisites no longer block readiness.
    pub satisfied: bool,
}

/// Collect edges once; preserve custom kinds and parallel dep/rel edges.
fn edges(state: &State) -> Vec<GraphEdge> {
    state
        .beads
        .values()
        .flat_map(|bead| {
            [(true, &bead.deps), (false, &bead.rels)]
                .into_iter()
                .flat_map(move |(blocking, links)| {
                    links.iter().map(move |(parent, kind)| GraphEdge {
                        from: parent.clone(),
                        to: bead.id.clone(),
                        kind: kind.clone(),
                        blocking,
                        satisfied: blocking
                            && state
                                .beads
                                .get(parent)
                                .is_none_or(|p| p.is_deleted() || p.status == Status::Closed),
                    })
                })
        })
        .collect()
}

/// Restrict the selection to the root's connected component. Closed/deleted
/// issues do not pull unrelated historical components into an active view.
pub fn component(state: &State, selected: &mut BTreeSet<String>, root: &str) {
    selected.insert(root.to_owned());
    let mut neighbors: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let all_edges = edges(state);
    for edge in &all_edges {
        if selected.contains(&edge.from) && selected.contains(&edge.to) {
            neighbors.entry(&edge.from).or_default().push(&edge.to);
            neighbors.entry(&edge.to).or_default().push(&edge.from);
        }
    }
    let mut found = BTreeSet::new();
    let mut pending = vec![root];
    while let Some(id) = pending.pop() {
        if found.insert(id.to_owned()) {
            pending.extend(neighbors.get(id).into_iter().flatten().copied());
        }
    }
    *selected = found;
}

impl IssueGraph {
    /// Include all ancestors as context. A ready view also follows blocking
    /// edges downstream, showing what the selected work can unblock.
    pub fn build(
        state: &State,
        selected: &BTreeSet<String>,
        downstream: bool,
        actor: &str,
        now: &str,
    ) -> Self {
        let all_edges = edges(state);
        let mut parents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for edge in &all_edges {
            parents.entry(&edge.to).or_default().push(&edge.from);
            if edge.blocking
                && state
                    .beads
                    .get(&edge.to)
                    .is_some_and(|b| !b.is_deleted() && b.status != Status::Closed)
            {
                dependents.entry(&edge.from).or_default().push(&edge.to);
            }
        }
        let mut included = selected.clone();
        if downstream {
            let mut pending: Vec<_> = selected.iter().map(String::as_str).collect();
            while let Some(id) = pending.pop() {
                for next in dependents.get(id).into_iter().flatten() {
                    if included.insert((*next).to_owned()) {
                        pending.push(next);
                    }
                }
            }
        }
        let mut pending: Vec<_> = included.iter().cloned().collect();
        while let Some(id) = pending.pop() {
            for parent in parents.get(id.as_str()).into_iter().flatten() {
                if included.insert((*parent).to_owned()) {
                    pending.push((*parent).to_owned());
                }
            }
        }
        let mut nodes: Vec<_> = included
            .iter()
            .map(|id| {
                let bead = state.beads.get(id);
                GraphNode {
                    id: id.clone(),
                    title: bead.map_or("(missing issue)", |b| b.title.as_str()).into(),
                    status: bead
                        .map_or("missing", |b| {
                            if b.is_deleted() {
                                "deleted"
                            } else {
                                b.status.as_str()
                            }
                        })
                        .into(),
                    priority: bead.map_or(3, |b| b.priority),
                    ready: bead.is_some_and(|b| state.is_ready_for(b, actor, now)),
                    waiting: bead.is_some_and(|b| {
                        !b.is_deleted() && b.status != Status::Closed && !state.deps_satisfied(b)
                    }),
                    stranded: bead.is_some_and(|b| state.offers_stranded(b, now)),
                    claimed_by: bead
                        .and_then(|b| b.claim.as_ref())
                        .filter(|c| c.is_live(now))
                        .map(|c| c.claimed_by.clone()),
                    context: !selected.contains(id),
                }
            })
            .collect();
        nodes.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.id.cmp(&b.id)));
        let edges = all_edges
            .into_iter()
            .filter(|e| included.contains(&e.from) && included.contains(&e.to))
            .collect();
        Self { nodes, edges }
    }

    pub fn text(&self, color: bool) -> String {
        if self.nodes.is_empty() {
            return "No matching issues.\n".into();
        }
        let mut out = String::new();
        let context = self.nodes.iter().filter(|n| n.context).count();
        writeln!(
            out,
            "Issue graph · {} selected · {context} context · {} links\n",
            self.nodes.len() - context,
            self.edges.len()
        )
        .unwrap();
        let indices: BTreeMap<_, _> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.as_str(), i))
            .collect();
        let mut children = vec![Vec::new(); self.nodes.len()];
        let mut has_parent = vec![false; self.nodes.len()];
        for edge in &self.edges {
            let from = indices[edge.from.as_str()];
            let to = indices[edge.to.as_str()];
            children[from].push((to, edge));
            has_parent[to] = true;
        }
        for links in &mut children {
            links.sort_by_key(|(to, edge)| (*to, !edge.blocking, &edge.kind));
        }
        // Start with connected roots, then isolated issues, then any rootless
        // cycles. Iterative traversal visits every edge and expands each node
        // once, avoiding exponential diamonds and recursive stack overflow.
        let mut roots: Vec<_> = (0..self.nodes.len()).collect();
        roots.sort_by_key(|&i| (has_parent[i], children[i].is_empty(), i));
        let mut seen = BTreeSet::new();
        for root in roots {
            if seen.contains(&root) {
                continue;
            }
            let mut pending = vec![(root, String::new(), true, None::<&GraphEdge>, 0)];
            while let Some((index, prefix, last, edge, depth)) = pending.pop() {
                let node = &self.nodes[index];
                let repeated = seen.contains(&index);
                out.push_str(&prefix);
                if let Some(edge) = edge {
                    out.push_str(if last { "└── " } else { "├── " });
                    out.push_str(&edge.label());
                    out.push(' ');
                }
                if repeated {
                    writeln!(
                        out,
                        "↩ {}  {}",
                        single_line(&node.id),
                        single_line(&node.title)
                    )
                    .unwrap();
                    continue;
                }
                // Long prerequisite chains should not march off the terminal.
                // Leave this node unvisited so a later root expands it fully.
                if depth == 4 {
                    writeln!(
                        out,
                        "↪ {}  {}",
                        single_line(&node.id),
                        single_line(&node.title)
                    )
                    .unwrap();
                    continue;
                }
                seen.insert(index);
                let label = format!(
                    "{} {}  {} · p{} · {}{}",
                    node.symbol(),
                    single_line(&node.title),
                    single_line(&node.id),
                    node.priority,
                    node.detail(),
                    if node.context { " [context]" } else { "" }
                );
                if color {
                    writeln!(out, "\x1b[{}m{label}\x1b[0m", node.color()).unwrap();
                } else {
                    writeln!(out, "{label}").unwrap();
                }
                let child_prefix = if edge.is_some() {
                    format!("{prefix}{}", if last { "    " } else { "│   " })
                } else {
                    prefix
                };
                for (i, (child, link)) in children[index].iter().enumerate().rev() {
                    pending.push((
                        *child,
                        child_prefix.clone(),
                        i + 1 == children[index].len(),
                        Some(*link),
                        depth + 1,
                    ));
                }
            }
            out.push('\n');
        }
        out.push_str(
            "→ blocks / ✓→ satisfied dependency · ┄ non-blocking relation\n↩ already shown · ↪ continues below\n",
        );
        if context > 0 {
            out.push_str("[context] related issue outside the selection\n");
        }
        out
    }

    pub fn mermaid(&self) -> String {
        let mut out = String::from("flowchart LR\n");
        let indices: BTreeMap<_, _> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.as_str(), i))
            .collect();
        for (i, node) in self.nodes.iter().enumerate() {
            let label = format!("{}: {}", node.id, node.title);
            let detail = format!(
                "p{} · {}{}",
                node.priority,
                node.detail(),
                if node.context { " · context" } else { "" }
            );
            writeln!(
                out,
                "  n{i}[\"{}<br/>{}\"]:::{}",
                mermaid_escape(&label),
                mermaid_escape(&detail),
                node.class()
            )
            .unwrap();
        }
        for edge in &self.edges {
            let from = indices[edge.from.as_str()];
            let to = indices[edge.to.as_str()];
            let arrow = if edge.blocking { "-->" } else { "-.->" };
            writeln!(
                out,
                "  n{from} {arrow}|\"{}\"| n{to}",
                mermaid_escape(&edge.description())
            )
            .unwrap();
        }
        out.push_str(
            "  classDef ready fill:#ecfdf5,stroke:#047857,color:#064e3b\n\
  classDef waiting fill:#fffbeb,stroke:#b45309,color:#78350f\n\
  classDef active fill:#eff6ff,stroke:#2563eb,color:#1e3a8a\n\
  classDef quiet fill:#f8fafc,stroke:#94a3b8,color:#475569\n",
        );
        out
    }
}

impl GraphEdge {
    fn description(&self) -> String {
        let label = if self.blocking {
            if self.satisfied {
                "satisfied"
            } else {
                "blocks"
            }
        } else {
            "relation"
        };
        if self.blocking && self.kind == "blocks" {
            label.into()
        } else {
            format!("{label}: {}", single_line(&self.kind))
        }
    }

    fn label(&self) -> String {
        if self.blocking {
            let arrow = if self.satisfied { "✓→" } else { "→" };
            if self.kind == "blocks" {
                arrow.into()
            } else {
                format!("{arrow} ({})", single_line(&self.kind))
            }
        } else {
            format!("┄ ({})", single_line(&self.kind))
        }
    }
}

impl GraphNode {
    fn detail(&self) -> String {
        let mut parts = vec![self.status.clone()];
        if self.ready {
            parts.push("ready".into());
        }
        if self.waiting {
            parts.push("waiting".into());
        }
        if self.stranded {
            parts.push("stranded".into());
        }
        if let Some(actor) = &self.claimed_by {
            parts.push(format!("claimed: {}", single_line(actor)));
        }
        parts.join(" · ")
    }

    fn symbol(&self) -> &str {
        match self.status.as_str() {
            "closed" => "✓",
            "deleted" | "missing" => "−",
            _ if self.ready => "●",
            _ if self.waiting || self.status == "blocked" => "◌",
            "doing" => "◆",
            "review" => "◇",
            _ => "○",
        }
    }

    fn class(&self) -> &str {
        if self.ready {
            "ready"
        } else if self.waiting || self.status == "blocked" {
            "waiting"
        } else if self.status == "doing" || self.status == "review" {
            "active"
        } else {
            "quiet"
        }
    }

    fn color(&self) -> &str {
        match self.class() {
            "ready" => "32",
            "waiting" => "33",
            "active" => "36",
            _ => "2",
        }
    }
}

/// Titles are untrusted text: never emit terminal controls or extra lines.
fn single_line(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Mermaid's decimal entities protect syntax as well as HTML label content.
fn mermaid_escape(value: &str) -> String {
    let mut out = String::new();
    for c in single_line(value).chars() {
        if c.is_alphanumeric() || " .,:_-/()".contains(c) {
            out.push(c);
        } else {
            write!(out, "#{};", c as u32).unwrap();
        }
    }
    out
}
