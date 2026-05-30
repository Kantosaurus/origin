// SPDX-License-Identifier: Apache-2.0
//! Dependency-free renderer for a useful subset of mermaid flowcharts to ASCII.
//!
//! `jcode` self-hosts a mermaid renderer so it can draw the flowcharts models
//! emit without shelling out to a browser or a JS toolchain. This crate brings
//! the same capability to `origin`: parse a small, common subset of mermaid
//! `graph`/`flowchart` syntax and render a readable, deterministic ASCII view —
//! pure `std`, no I/O, no async, no external crates.
//!
//! Supported input:
//! - headers `graph TD`, `graph LR`, `flowchart TD`, `flowchart LR`
//! - node definitions with labels: `A[Box]`, `B(Round)`, `C{Diamond}`
//! - edges: `A-->B`, `A--text-->B`, `A---B`
//!
//! Any line that is not recognized (comments, styling, subgraphs, class defs)
//! is ignored gracefully rather than erroring.
//!
//! ```
//! use origin_mermaid::{parse, render_ascii};
//!
//! let d = parse("graph TD\n A[Start] --> B{Choice}\n B -- yes --> C[Done]").unwrap();
//! let art = render_ascii(&d);
//! assert!(art.contains("Start"));
//! assert!(art.contains("-->"));
//! ```

#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::fmt;

/// Layout direction declared by the diagram header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// `TD` / `TB`: nodes flow from top to bottom.
    TopDown,
    /// `LR`: nodes flow from left to right.
    LeftRight,
}

/// Geometric shape of a node, taken from its mermaid bracket style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeShape {
    /// `[label]` — a rectangle.
    Box,
    /// `(label)` — a rounded rectangle.
    Round,
    /// `{label}` — a decision diamond.
    Diamond,
}

impl NodeShape {
    /// The opening/closing delimiter pair for this shape (`[`/`]`, etc.).
    #[must_use]
    const fn delims(self) -> (char, char) {
        match self {
            Self::Box => ('[', ']'),
            Self::Round => ('(', ')'),
            Self::Diamond => ('{', '}'),
        }
    }
}

/// A flowchart node with its identifier, display label, and shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Stable identifier used to reference the node in edges.
    pub id: String,
    /// Human-readable label; defaults to the id when none was declared.
    pub label: String,
    /// Declared shape; defaults to [`NodeShape::Box`].
    pub shape: NodeShape,
}

/// A directed (or plain) edge between two nodes, with an optional label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    /// Source node id.
    pub from: String,
    /// Destination node id.
    pub to: String,
    /// Edge label text, if the edge carried `-- text -->`.
    pub label: Option<String>,
}

/// A parsed flowchart: a direction plus its nodes and edges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagram {
    /// Declared layout direction.
    pub direction: Direction,
    /// Nodes in first-seen order (deduplicated by id).
    pub nodes: Vec<Node>,
    /// Edges in first-seen order.
    pub edges: Vec<Edge>,
}

/// Errors that can occur while parsing a mermaid source string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MermaidError {
    /// The input was empty or contained no meaningful (non-blank) lines.
    Empty,
    /// The header line was present but not a supported diagram kind/direction.
    Unsupported(String),
}

impl fmt::Display for MermaidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "mermaid source is empty"),
            Self::Unsupported(what) => write!(f, "unsupported mermaid construct: {what}"),
        }
    }
}

impl std::error::Error for MermaidError {}

/// Parse a mermaid flowchart subset into a [`Diagram`].
///
/// The first non-blank line must be a supported header (`graph`/`flowchart`
/// followed by `TD`/`TB`/`LR`). Subsequent lines may declare nodes and edges;
/// unrecognized lines are skipped. Nodes referenced only by edges are created
/// implicitly with a default [`NodeShape::Box`] and a label equal to their id.
///
/// # Errors
///
/// Returns [`MermaidError::Empty`] when the source has no meaningful lines, and
/// [`MermaidError::Unsupported`] when the header is missing or not a supported
/// diagram kind/direction.
pub fn parse(src: &str) -> Result<Diagram, MermaidError> {
    let mut lines = src
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("%%"));

    let header = lines.next().ok_or(MermaidError::Empty)?;
    let direction = parse_header(header)?;

    let mut builder = Builder::new(direction);
    for line in lines {
        builder.consume(line);
    }
    Ok(builder.finish())
}

/// Render a [`Diagram`] as a readable, deterministic ASCII adjacency view.
///
/// The output lists every node as a boxed label, then a layered adjacency block
/// where each source node's outgoing edges are shown with `-->` arrows and any
/// edge labels in `-- … -->` form. Ordering is deterministic: nodes appear in
/// declaration order and each node's targets in edge-declaration order, so the
/// same diagram always renders identically.
#[must_use]
pub fn render_ascii(d: &Diagram) -> String {
    let mut out = String::new();
    let dir = match d.direction {
        Direction::TopDown => "TD",
        Direction::LeftRight => "LR",
    };
    out.push_str("flowchart ");
    out.push_str(dir);
    out.push('\n');

    out.push_str("\nNodes:\n");
    for node in &d.nodes {
        out.push_str("  ");
        out.push_str(&boxed(&node.label, node.shape));
        out.push('\n');
    }

    out.push_str("\nEdges:\n");
    if d.edges.is_empty() {
        out.push_str("  (none)\n");
        return out;
    }

    // Group edges by source, preserving the order sources first appear in the
    // node list (declaration order), then edge-declaration order within each.
    for node in &d.nodes {
        let outgoing: Vec<&Edge> = d.edges.iter().filter(|e| e.from == node.id).collect();
        if outgoing.is_empty() {
            continue;
        }
        let label_of = |id: &str| -> String {
            d.nodes
                .iter()
                .find(|n| n.id == id)
                .map_or_else(|| id.to_string(), |n| n.label.clone())
        };
        out.push_str("  ");
        out.push_str(&label_of(&node.id));
        out.push('\n');
        for edge in outgoing {
            let arrow = edge.label.as_ref().map_or_else(
                || "    -->".to_string(),
                |text| format!("    -- {text} -->"),
            );
            out.push_str(&arrow);
            out.push(' ');
            out.push_str(&label_of(&edge.to));
            out.push('\n');
        }
    }
    out
}

/// Wrap `label` in an ASCII box matching `shape`.
fn boxed(label: &str, shape: NodeShape) -> String {
    match shape {
        NodeShape::Box => format!("[ {label} ]"),
        NodeShape::Round => format!("( {label} )"),
        NodeShape::Diamond => format!("< {label} >"),
    }
}

/// Parse the header line into a [`Direction`].
fn parse_header(line: &str) -> Result<Direction, MermaidError> {
    let mut words = line.split_whitespace();
    let kind = words.next().unwrap_or_default().to_ascii_lowercase();
    if kind != "graph" && kind != "flowchart" {
        return Err(MermaidError::Unsupported(line.to_string()));
    }
    match words.next().map(str::to_ascii_uppercase).as_deref() {
        Some("TD" | "TB") => Ok(Direction::TopDown),
        Some("LR") => Ok(Direction::LeftRight),
        other => Err(MermaidError::Unsupported(
            other.map_or_else(|| line.to_string(), ToString::to_string),
        )),
    }
}

/// Incrementally assembles a [`Diagram`] from parsed lines, deduplicating nodes
/// by id and upgrading implicit (edge-only) nodes when a definition is seen.
struct Builder {
    direction: Direction,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    seen: BTreeSet<String>,
}

impl Builder {
    const fn new(direction: Direction) -> Self {
        Self {
            direction,
            nodes: Vec::new(),
            edges: Vec::new(),
            seen: BTreeSet::new(),
        }
    }

    /// Record a node definition (or reference), keeping the richest known label.
    fn upsert_node(&mut self, id: &str, label: Option<String>, shape: Option<NodeShape>) {
        if let Some(existing) = self.nodes.iter_mut().find(|n| n.id == id) {
            if let Some(label) = label {
                existing.label = label;
            }
            if let Some(shape) = shape {
                existing.shape = shape;
            }
            return;
        }
        self.seen.insert(id.to_string());
        self.nodes.push(Node {
            id: id.to_string(),
            label: label.unwrap_or_else(|| id.to_string()),
            shape: shape.unwrap_or(NodeShape::Box),
        });
    }

    /// Parse one body line as either an edge or a bare node definition.
    fn consume(&mut self, line: &str) {
        if let Some((from_tok, label, to_tok)) = split_edge(line) {
            let (from_id, from_label, from_shape) = parse_node_token(from_tok);
            let (to_id, to_label, to_shape) = parse_node_token(to_tok);
            if from_id.is_empty() || to_id.is_empty() {
                return;
            }
            self.upsert_node(&from_id, from_label, from_shape);
            self.upsert_node(&to_id, to_label, to_shape);
            self.edges.push(Edge {
                from: from_id,
                to: to_id,
                label,
            });
        } else {
            let (id, label, shape) = parse_node_token(line);
            if !id.is_empty() && (label.is_some() || shape.is_some()) {
                self.upsert_node(&id, label, shape);
            }
        }
    }

    fn finish(self) -> Diagram {
        Diagram {
            direction: self.direction,
            nodes: self.nodes,
            edges: self.edges,
        }
    }
}

/// Split a line into `(left_token, optional_label, right_token)` if it contains
/// a supported edge connector. Recognizes `-->`, `---`, and `-- text -->`.
///
/// Returns `None` when no connector is present.
fn split_edge(line: &str) -> Option<(&str, Option<String>, &str)> {
    // Labeled arrow: `A -- text --> B`. Find the first `--` followed later by
    // `-->`, with text in between.
    if let Some(arrow_idx) = line.find("-->") {
        let (left, right) = line.split_at(arrow_idx);
        let right = &right["-->".len()..];
        // Does the left side carry an inline `-- text` label?
        if let Some(dash_idx) = left.find("--") {
            let head = left[..dash_idx].trim();
            let mid = left[dash_idx + "--".len()..].trim();
            if !mid.is_empty() {
                return Some((head, Some(mid.to_string()), right.trim()));
            }
            // `A---->B` style: treat the leading `--` as part of the arrow.
            return Some((head, None, right.trim()));
        }
        return Some((left.trim(), None, right.trim()));
    }
    // Plain undirected link `A --- B` (three or more dashes, no arrowhead).
    if let Some(idx) = line.find("---") {
        let left = line[..idx].trim();
        // Skip past the whole dash run to find the right token.
        let rest = &line[idx..];
        let after = rest.trim_start_matches('-').trim();
        if !left.is_empty() {
            return Some((left, None, after));
        }
    }
    None
}

/// Parse a node token like `A[Label]`, `B(Round)`, `C{Diamond}`, or bare `D`
/// into `(id, optional_label, optional_shape)`.
fn parse_node_token(token: &str) -> (String, Option<String>, Option<NodeShape>) {
    let token = token.trim();
    for shape in [NodeShape::Box, NodeShape::Round, NodeShape::Diamond] {
        let (open, close) = shape.delims();
        if let Some(open_idx) = token.find(open) {
            if token.ends_with(close) && token.len() > open_idx + 1 {
                let id = token[..open_idx].trim().to_string();
                let inner = &token[open_idx + open.len_utf8()..token.len() - close.len_utf8()];
                let label = strip_quotes(inner.trim());
                return (id, Some(label), Some(shape));
            }
        }
    }
    (token.to_string(), None, None)
}

/// Drop a single pair of surrounding double quotes, if present.
fn strip_quotes(s: &str) -> String {
    s.strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(s)
        .to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn parses_three_node_top_down_graph() {
        let d = parse("graph TD\n A[Start] --> B[Middle]\n B --> C[End]").unwrap();
        assert_eq!(d.direction, Direction::TopDown);
        assert_eq!(d.nodes.len(), 3);
        assert_eq!(d.edges.len(), 2);
        assert_eq!(d.nodes[0].id, "A");
        assert_eq!(d.nodes[0].label, "Start");
        assert_eq!(d.edges[0].from, "A");
        assert_eq!(d.edges[0].to, "B");
    }

    #[test]
    fn parses_edge_labels() {
        let d = parse("flowchart LR\n A -- yes --> B\n A--no-->C").unwrap();
        assert_eq!(d.direction, Direction::LeftRight);
        assert_eq!(d.edges[0].label.as_deref(), Some("yes"));
        assert_eq!(d.edges[1].label.as_deref(), Some("no"));
        assert_eq!(d.edges[1].to, "C");
    }

    #[test]
    fn parses_diamond_and_round_shapes() {
        let d = parse("graph TD\n A(Round) --> B{Decision}").unwrap();
        let a = d.nodes.iter().find(|n| n.id == "A").unwrap();
        let b = d.nodes.iter().find(|n| n.id == "B").unwrap();
        assert_eq!(a.shape, NodeShape::Round);
        assert_eq!(a.label, "Round");
        assert_eq!(b.shape, NodeShape::Diamond);
        assert_eq!(b.label, "Decision");
    }

    #[test]
    fn parses_plain_undirected_link() {
        let d = parse("graph LR\n A --- B").unwrap();
        assert_eq!(d.edges.len(), 1);
        assert_eq!(d.edges[0].from, "A");
        assert_eq!(d.edges[0].to, "B");
        assert!(d.edges[0].label.is_none());
    }

    #[test]
    fn render_contains_labels_and_arrows() {
        let d = parse("graph TD\n A[Start] --> B{Choice}\n B -- yes --> C[Done]").unwrap();
        let art = render_ascii(&d);
        assert!(art.contains("Start"));
        assert!(art.contains("Choice"));
        assert!(art.contains("Done"));
        assert!(art.contains("-->"));
        assert!(art.contains("-- yes -->"));
    }

    #[test]
    fn render_is_deterministic() {
        let d = parse("graph TD\n A --> B\n A --> C").unwrap();
        assert_eq!(render_ascii(&d), render_ascii(&d));
    }

    #[test]
    fn empty_input_errors() {
        assert_eq!(parse(""), Err(MermaidError::Empty));
        assert_eq!(parse("   \n  \n"), Err(MermaidError::Empty));
        assert_eq!(parse("%% only a comment\n"), Err(MermaidError::Empty));
    }

    #[test]
    fn unsupported_header_errors() {
        assert!(matches!(
            parse("sequenceDiagram\n A->>B: hi"),
            Err(MermaidError::Unsupported(_))
        ));
        assert!(matches!(
            parse("graph XY\n A --> B"),
            Err(MermaidError::Unsupported(_))
        ));
    }

    #[test]
    fn unsupported_lines_ignored_gracefully() {
        let d = parse(
            "graph TD\n %% a comment\n classDef foo fill:#f00\n A[Real] --> B[Node]\n subgraph s",
        )
        .unwrap();
        // Only the real edge/nodes survive; junk lines are skipped.
        assert_eq!(d.edges.len(), 1);
        assert!(d.nodes.iter().any(|n| n.label == "Real"));
    }

    #[test]
    fn implicit_nodes_get_default_box_shape() {
        let d = parse("graph TD\n A --> B").unwrap();
        let a = d.nodes.iter().find(|n| n.id == "A").unwrap();
        assert_eq!(a.shape, NodeShape::Box);
        assert_eq!(a.label, "A");
    }

    #[test]
    fn quoted_labels_are_unquoted() {
        let d = parse("graph TD\n A[\"Quoted Label\"] --> B").unwrap();
        let a = d.nodes.iter().find(|n| n.id == "A").unwrap();
        assert_eq!(a.label, "Quoted Label");
    }

    #[test]
    fn error_display_is_human_readable() {
        assert_eq!(format!("{}", MermaidError::Empty), "mermaid source is empty");
        assert_eq!(
            format!("{}", MermaidError::Unsupported("pie".to_string())),
            "unsupported mermaid construct: pie"
        );
    }
}
