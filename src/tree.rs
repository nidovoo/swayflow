//! Typed view of the JSON tree returned by Sway's `GET_TREE`
//! and the tree-walking helpers (`walk`, `locate_*`, `collect_leaves`).

use serde::Deserialize;

use crate::geom::Rect;

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct RawRect {
    #[serde(default)]
    pub x: i64,
    #[serde(default)]
    pub y: i64,
    #[serde(default)]
    pub width: i64,
    #[serde(default)]
    pub height: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WindowProperties {
    #[serde(default)]
    pub class: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Node {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub rect: RawRect,
    #[serde(default)]
    pub window_rect: Option<RawRect>,
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub floating_nodes: Vec<Node>,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub fullscreen_mode: Option<i64>,
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub focus: Vec<i64>,
    #[serde(default)]
    pub app_id: Option<String>,
    #[serde(default)]
    pub window_properties: Option<WindowProperties>,
    #[serde(default)]
    pub num: Option<i64>,
    #[serde(default)]
    pub current_workspace: Option<String>,
}

impl Node {
    /// Python `rect_of(node)`.
    pub fn bounds(&self) -> Rect {
        Rect::new(self.rect.x, self.rect.y, self.rect.width, self.rect.height)
    }

    pub fn is_workspace(&self) -> bool {
        self.kind == "workspace"
    }

    /// `type` is `con` or `floating_con` (i.e. a window or a container of windows).
    pub fn is_window_kind(&self) -> bool {
        matches!(self.kind.as_str(), "con" | "floating_con")
    }

    pub fn is_fullscreen(&self) -> bool {
        self.fullscreen_mode.unwrap_or(0) != 0
    }

    /// `app_id`, else the X11 class, else `"?"` (empty strings count as missing).
    pub fn window_key(&self) -> &str {
        let app_id = self.app_id.as_deref().filter(|s| !s.is_empty());
        let class = self
            .window_properties
            .as_ref()
            .and_then(|p| p.class.as_deref())
            .filter(|s| !s.is_empty());
        app_id.or(class).unwrap_or("?")
    }
}

/// One step of the depth-first traversal: the node and its ancestors (root first).
pub struct Visit<'a> {
    pub node: &'a Node,
    pub ancestors: Vec<&'a Node>,
}

pub struct Walk<'a> {
    stack: Vec<Visit<'a>>,
}

/// Pre-order traversal; children are visited as `nodes` first, then `floating_nodes`.
pub fn walk(root: &Node) -> Walk<'_> {
    Walk {
        stack: vec![Visit {
            node: root,
            ancestors: Vec::new(),
        }],
    }
}

impl<'a> Iterator for Walk<'a> {
    type Item = Visit<'a>;

    fn next(&mut self) -> Option<Visit<'a>> {
        let visit = self.stack.pop()?;
        let mut path = visit.ancestors.clone();
        path.push(visit.node);
        // Pushed in reverse so that they are popped in document order.
        let children = visit
            .node
            .floating_nodes
            .iter()
            .rev()
            .chain(visit.node.nodes.iter().rev());
        for child in children {
            self.stack.push(Visit {
                node: child,
                ancestors: path.clone(),
            });
        }
        Some(visit)
    }
}

/// Closest node of the given kind, starting with `node` itself (if given)
/// and continuing with its ancestors from the nearest to the farthest.
fn nearest<'a>(ancestors: &[&'a Node], node: Option<&'a Node>, kind: &str) -> Option<&'a Node> {
    node.into_iter()
        .chain(ancestors.iter().rev().copied())
        .find(|n| n.kind == kind)
}

pub struct Focus<'a> {
    pub workspace: &'a Node,
    pub output: &'a Node,
    /// The focused window, if the focus is on a window rather than on an empty workspace.
    pub leaf: Option<&'a Node>,
}

pub fn locate_focus(tree: &Node) -> Option<Focus<'_>> {
    for Visit { node, ancestors } in walk(tree) {
        if !node.focused {
            continue;
        }
        let workspace = nearest(&ancestors, Some(node), "workspace");
        let output = nearest(&ancestors, Some(node), "output");
        if let (Some(workspace), Some(output)) = (workspace, output) {
            let leaf = if node.is_window_kind() { Some(node) } else { None };
            return Some(Focus {
                workspace,
                output,
                leaf,
            });
        }
    }
    None
}

/// Finds the container with `con_id` together with the workspace that holds it.
pub fn locate_window(tree: &Node, con_id: i64) -> Option<(&Node, &Node)> {
    let Visit { node, ancestors } = walk(tree).find(|v| v.node.id == con_id)?;
    let workspace = nearest(&ancestors, None, "workspace")?;
    Some((workspace, node))
}

/// All windows below `nodes` (containers are descended into, only leaves are kept).
pub fn collect_leaves(nodes: &[Node]) -> Vec<&Node> {
    let mut acc = Vec::new();
    push_leaves(nodes, &mut acc);
    acc
}

fn push_leaves<'a>(nodes: &'a [Node], acc: &mut Vec<&'a Node>) {
    for n in nodes {
        if n.nodes.is_empty() && n.floating_nodes.is_empty() {
            if n.is_window_kind() {
                acc.push(n);
            }
        } else {
            push_leaves(&n.nodes, acc);
            push_leaves(&n.floating_nodes, acc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected values below were produced by the reference Python implementation (swayflow_ver4.py).

    const TREE: &str = r##"{"id":1,"type":"root","name":"root","rect":{"x":0,"y":0,"width":1920,"height":1080},"nodes":[{"id":2,"type":"output","name":"DP-1","rect":{"x":0,"y":0,"width":1920,"height":1080},"nodes":[{"id":3,"type":"workspace","name":"1","num":1,"rect":{"x":0,"y":0,"width":1920,"height":1080},"nodes":[{"id":10,"type":"con","rect":{"x":0,"y":0,"width":10,"height":10},"nodes":[],"floating_nodes":[],"focused":true,"app_id":"firefox"},{"id":11,"type":"con","rect":{"x":0,"y":0,"width":10,"height":10},"nodes":[],"floating_nodes":[],"focused":false,"window_properties":{"class":"Steam"}},{"id":20,"type":"con","rect":{"x":0,"y":0,"width":10,"height":10},"nodes":[{"id":21,"type":"con","rect":{"x":0,"y":0,"width":10,"height":10},"nodes":[],"floating_nodes":[],"focused":false},{"id":22,"type":"con","rect":{"x":0,"y":0,"width":10,"height":10},"nodes":[],"floating_nodes":[],"focused":false}],"floating_nodes":[],"focused":false}],"floating_nodes":[{"id":12,"type":"floating_con","rect":{"x":0,"y":0,"width":10,"height":10},"nodes":[],"floating_nodes":[],"focused":false,"app_id":""},{"id":13,"type":"floating_con","rect":{"x":0,"y":0,"width":10,"height":10},"nodes":[{"id":14,"type":"con","rect":{"x":0,"y":0,"width":10,"height":10},"nodes":[],"floating_nodes":[],"focused":false},{"id":15,"type":"con","rect":{"x":0,"y":0,"width":10,"height":10},"nodes":[],"floating_nodes":[],"focused":false}],"floating_nodes":[],"focused":false}],"focus":[10,11,20]},{"id":4,"type":"workspace","name":"2","num":2,"rect":{"x":0,"y":0,"width":1920,"height":1080},"nodes":[],"floating_nodes":[],"focus":[]}],"floating_nodes":[],"focus":[3,4],"current_workspace":"1"}],"floating_nodes":[],"focus":[2]}"##;
    const EMPTY_FOCUS: &str = r##"{"id":1,"type":"root","name":"root","rect":{"x":0,"y":0,"width":1920,"height":1080},"nodes":[{"id":2,"type":"output","name":"DP-1","rect":{"x":0,"y":0,"width":1920,"height":1080},"nodes":[{"id":3,"type":"workspace","name":"1","num":1,"rect":{"x":0,"y":0,"width":1920,"height":1080},"nodes":[{"id":10,"type":"con","rect":{"x":0,"y":0,"width":10,"height":10},"nodes":[],"floating_nodes":[],"focused":false,"app_id":"firefox"},{"id":11,"type":"con","rect":{"x":0,"y":0,"width":10,"height":10},"nodes":[],"floating_nodes":[],"focused":false,"window_properties":{"class":"Steam"}},{"id":20,"type":"con","rect":{"x":0,"y":0,"width":10,"height":10},"nodes":[{"id":21,"type":"con","rect":{"x":0,"y":0,"width":10,"height":10},"nodes":[],"floating_nodes":[],"focused":false},{"id":22,"type":"con","rect":{"x":0,"y":0,"width":10,"height":10},"nodes":[],"floating_nodes":[],"focused":false}],"floating_nodes":[],"focused":false}],"floating_nodes":[{"id":12,"type":"floating_con","rect":{"x":0,"y":0,"width":10,"height":10},"nodes":[],"floating_nodes":[],"focused":false,"app_id":""},{"id":13,"type":"floating_con","rect":{"x":0,"y":0,"width":10,"height":10},"nodes":[{"id":14,"type":"con","rect":{"x":0,"y":0,"width":10,"height":10},"nodes":[],"floating_nodes":[],"focused":false},{"id":15,"type":"con","rect":{"x":0,"y":0,"width":10,"height":10},"nodes":[],"floating_nodes":[],"focused":false}],"floating_nodes":[],"focused":false}],"focus":[10,11,20]},{"id":4,"type":"workspace","name":"2","num":2,"rect":{"x":0,"y":0,"width":1920,"height":1080},"nodes":[],"floating_nodes":[],"focus":[],"focused":true}],"floating_nodes":[],"focus":[3,4],"current_workspace":"1"}],"floating_nodes":[],"focus":[2]}"##;

    fn tree() -> Node {
        serde_json::from_str(TREE).expect("valid test tree")
    }

    #[test]
    fn walk_is_preorder_with_tiled_before_floating() {
        let tree = tree();
        let ids: Vec<i64> = walk(&tree).map(|v| v.node.id).collect();
        assert_eq!(ids, [1, 2, 3, 10, 11, 20, 21, 22, 12, 13, 14, 15, 4]);
    }

    #[test]
    fn walk_reports_ancestors_root_first() {
        let tree = tree();
        let visit = walk(&tree).find(|v| v.node.id == 15).expect("node 15");
        let chain: Vec<i64> = visit.ancestors.iter().map(|n| n.id).collect();
        assert_eq!(chain, [1, 2, 3, 13]);
    }

    #[test]
    fn locate_focus_finds_workspace_output_and_leaf() {
        let tree = tree();
        let focus = locate_focus(&tree).expect("focus");
        assert_eq!(
            (focus.workspace.id, focus.output.id, focus.leaf.map(|n| n.id)),
            (3, 2, Some(10))
        );
    }

    #[test]
    fn locate_focus_on_empty_workspace_has_no_leaf() {
        let tree: Node = serde_json::from_str(EMPTY_FOCUS).expect("valid test tree");
        let focus = locate_focus(&tree).expect("focus");
        assert_eq!(
            (focus.workspace.id, focus.output.id, focus.leaf.map(|n| n.id)),
            (4, 2, None)
        );
    }

    #[test]
    fn locate_focus_requires_workspace_and_output() {
        let orphan: Node = serde_json::from_str(
            r##"{"id":1,"type":"root","nodes":[{"id":9,"type":"con","focused":true}]}"##,
        )
        .expect("valid test tree");
        assert!(locate_focus(&orphan).is_none());
    }

    #[test]
    fn locate_window_returns_the_enclosing_workspace() {
        let tree = tree();
        let ids = |found: Option<(&Node, &Node)>| found.map(|(ws, n)| (ws.id, n.id));
        assert_eq!(ids(locate_window(&tree, 12)), Some((3, 12)));
        assert_eq!(ids(locate_window(&tree, 15)), Some((3, 15)));
        assert_eq!(ids(locate_window(&tree, 1)), None);
        assert_eq!(ids(locate_window(&tree, 999)), None);
    }

    #[test]
    fn collect_leaves_descends_into_containers() {
        let tree = tree();
        let ws = locate_focus(&tree).expect("focus").workspace;
        let tiled: Vec<i64> = collect_leaves(&ws.nodes).iter().map(|n| n.id).collect();
        let floating: Vec<i64> = collect_leaves(&ws.floating_nodes).iter().map(|n| n.id).collect();
        assert_eq!(tiled, [10, 11, 21, 22]);
        assert_eq!(floating, [12, 14, 15]);
    }

    #[test]
    fn window_key_prefers_app_id_then_class_then_question_mark() {
        let tree = tree();
        let key_of = |id: i64| {
            walk(&tree)
                .find(|v| v.node.id == id)
                .map(|v| v.node.window_key().to_owned())
                .expect("node")
        };
        assert_eq!(key_of(10), "firefox");
        assert_eq!(key_of(11), "Steam");
        assert_eq!(key_of(12), "?");
        assert_eq!(key_of(21), "?");
    }

    #[test]
    fn node_helpers() {
        let node: Node = serde_json::from_str(
            r##"{"id":5,"type":"floating_con","rect":{"x":1,"y":2,"width":3,"height":4},"fullscreen_mode":1}"##,
        )
        .expect("valid node");
        assert!(node.is_window_kind());
        assert!(node.is_fullscreen());
        assert!(!node.is_workspace());
        assert_eq!(node.bounds(), Rect::new(1, 2, 3, 4));
        assert!(!Node::default().is_fullscreen());
    }
}
