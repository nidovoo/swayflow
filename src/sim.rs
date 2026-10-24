//! Simulation of Sway's tiling tree, used to work out which `split h|v`
//! commands are needed when floating windows are returned to tiling so that
//! they end up arranged like a dwindle/spiral layout.

use std::collections::HashMap;

use crate::tree::{collect_leaves, Node};

/// A window is split vertically when it is taller than `width * SPLIT_RATIO`.
const SPLIT_RATIO: f64 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    SplitH,
    SplitV,
    /// tabbed, stacked, leaf — every child gets the full rect.
    Other,
}

impl Layout {
    fn from_name(name: Option<&str>) -> Layout {
        match name {
            Some("splith") => Layout::SplitH,
            Some("splitv") => Layout::SplitV,
            _ => Layout::Other,
        }
    }
}

type FRect = (f64, f64, f64, f64);

struct SimNode {
    id: i64,
    layout: Layout,
    parent: Option<usize>,
    kids: Vec<usize>,
    rect: FRect,
}

/// Arena-based tree (indices instead of parent pointers).
#[derive(Default)]
struct Sim {
    nodes: Vec<SimNode>,
}

impl Sim {
    fn add(&mut self, id: i64, layout: Layout, parent: Option<usize>) -> usize {
        self.nodes.push(SimNode {
            id,
            layout,
            parent,
            kids: Vec::new(),
            rect: (0.0, 0.0, 0.0, 0.0),
        });
        self.nodes.len() - 1
    }

    fn build(&mut self, node: &Node, parent: usize) -> usize {
        let layout = if node.nodes.is_empty() {
            Layout::Other
        } else {
            Layout::from_name(node.layout.as_deref())
        };
        let idx = self.add(node.id, layout, Some(parent));
        for child in &node.nodes {
            let kid = self.build(child, idx);
            self.nodes[idx].kids.push(kid);
        }
        idx
    }

    fn arrange(&mut self, idx: usize, rect: FRect) {
        self.nodes[idx].rect = rect;
        let (x, y, w, h) = rect;
        let layout = self.nodes[idx].layout;
        let kids = self.nodes[idx].kids.clone();
        let n = kids.len() as f64;
        for (i, kid) in kids.into_iter().enumerate() {
            let i = i as f64;
            let child_rect = match layout {
                Layout::SplitH => (x + w * i / n, y, w / n, h),
                Layout::SplitV => (x, y + h * i / n, w, h / n),
                Layout::Other => rect,
            };
            self.arrange(kid, child_rect);
        }
    }

    /// Makes `node` the sole child of a container with the given layout.
    fn split(&mut self, node: usize, layout: Layout) {
        let Some(parent) = self.nodes[node].parent else {
            return;
        };
        if self.nodes[parent].kids.len() == 1 {
            self.nodes[parent].layout = layout;
            return;
        }
        let wrapper = self.add(-1, layout, Some(parent));
        if let Some(pos) = self.nodes[parent].kids.iter().position(|&k| k == node) {
            self.nodes[parent].kids[pos] = wrapper;
        }
        self.nodes[wrapper].kids.push(node);
        self.nodes[node].parent = Some(wrapper);
    }
}

/// The tiled window Sway would insert next to: the leaf reached by following
/// the focus history down the tree, else the last leaf.
pub fn focus_inactive_leaf(ws: &Node) -> Option<i64> {
    for fid in &ws.focus {
        let Some(mut node) = ws.nodes.iter().find(|n| n.id == *fid) else {
            continue;
        };
        while let Some(first) = node.nodes.first() {
            let current = node;
            node = current
                .focus
                .iter()
                .find_map(|id| current.nodes.iter().find(|n| n.id == *id))
                .unwrap_or(first);
        }
        return Some(node.id);
    }
    collect_leaves(&ws.nodes).last().map(|n| n.id)
}

/// Sway commands that return `ids` (in order) to the tiling tree of `ws`,
/// splitting in the direction of the longer side, and finally restore focus.
pub fn retile_commands(ws: &Node, ids: &[i64], focused_id: Option<i64>) -> Vec<String> {
    let root_layout = match ws.layout.as_deref() {
        Some("tabbed" | "stacked") => Layout::Other,
        Some("splitv") => Layout::SplitV,
        _ => Layout::SplitH, // "splith" and anything unexpected
    };
    let mut sim = Sim::default();
    let root = sim.add(ws.id, root_layout, None);
    for child in &ws.nodes {
        let kid = sim.build(child, root);
        sim.nodes[root].kids.push(kid);
    }
    let by_id: HashMap<i64, usize> = sim
        .nodes
        .iter()
        .enumerate()
        .map(|(idx, n)| (n.id, idx))
        .collect();
    let mut reference = focus_inactive_leaf(ws).and_then(|id| by_id.get(&id).copied());

    let area = ws.bounds();
    let area = (area.x as f64, area.y as f64, area.w as f64, area.h as f64);
    let mut cmds = Vec::new();

    for (pos, &wid) in ids.iter().enumerate() {
        let leaf = sim.add(wid, Layout::Other, None);
        let ref_parent = reference.and_then(|r| sim.nodes[r].parent);
        let parent = ref_parent.unwrap_or(root);
        sim.nodes[leaf].parent = Some(parent);
        match (reference, ref_parent) {
            (Some(r), Some(_)) => {
                let at = sim.nodes[parent]
                    .kids
                    .iter()
                    .position(|&k| k == r)
                    .map_or(sim.nodes[parent].kids.len(), |p| p + 1);
                sim.nodes[parent].kids.insert(at, leaf);
            }
            _ => sim.nodes[parent].kids.push(leaf),
        }
        cmds.push(format!("[con_id={wid}] floating disable"));
        reference = Some(leaf);

        if pos + 1 < ids.len() {
            sim.arrange(root, area);
            let (_, _, w, h) = sim.nodes[leaf].rect;
            let wanted = if h > w * SPLIT_RATIO {
                Layout::SplitV
            } else {
                Layout::SplitH
            };
            cmds.push(format!("[con_id={wid}] focus"));
            if sim.nodes[parent].layout != wanted {
                let dir = if wanted == Layout::SplitV { "v" } else { "h" };
                cmds.push(format!("[con_id={wid}] split {dir}"));
                sim.split(leaf, wanted);
            }
        }
    }
    if let Some(id) = focused_id {
        cmds.push(format!("[con_id={id}] focus"));
    }
    cmds
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected values below were produced by the reference Python implementation (swayflow_ver4.py).

    fn parse(json: &str) -> Node {
        serde_json::from_str(json).expect("valid test tree")
    }

    #[test]
    fn retile_empty_workspace_matches_reference() {
        let ws = parse(r##"{"id":100,"type":"workspace","name":"100","layout":"splith","rect":{"x":0,"y":30,"width":1920,"height":1050},"nodes":[],"floating_nodes":[],"focus":[]}"##);
        let cmds = retile_commands(&ws, &[11, 12, 13, 14], Some(12));
        assert_eq!(cmds, ["[con_id=11] floating disable", "[con_id=11] focus", "[con_id=12] floating disable", "[con_id=12] focus", "[con_id=12] split v", "[con_id=13] floating disable", "[con_id=13] focus", "[con_id=13] split h", "[con_id=14] floating disable", "[con_id=12] focus"]);
    }

    #[test]
    fn retile_nested_tree_matches_reference() {
        let ws = parse(r##"{"id":100,"type":"workspace","name":"100","layout":"splith","rect":{"x":0,"y":30,"width":1920,"height":1050},"nodes":[{"id":5,"type":"con","rect":{"x":0,"y":0,"width":100,"height":100},"nodes":[],"floating_nodes":[],"focus":[],"focused":false},{"id":6,"type":"con","layout":"splitv","rect":{"x":0,"y":0,"width":100,"height":100},"nodes":[{"id":7,"type":"con","rect":{"x":0,"y":0,"width":100,"height":100},"nodes":[],"floating_nodes":[],"focus":[],"focused":false},{"id":8,"type":"con","rect":{"x":0,"y":0,"width":100,"height":100},"nodes":[],"floating_nodes":[],"focus":[],"focused":false}],"floating_nodes":[],"focus":[8,7]}],"floating_nodes":[],"focus":[6,5]}"##);
        let cmds = retile_commands(&ws, &[21, 22, 23], None);
        assert_eq!(cmds, ["[con_id=21] floating disable", "[con_id=21] focus", "[con_id=21] split h", "[con_id=22] floating disable", "[con_id=22] focus", "[con_id=23] floating disable"]);
    }

    #[test]
    fn retile_tabbed_workspace_matches_reference() {
        let ws = parse(r##"{"id":100,"type":"workspace","name":"100","layout":"tabbed","rect":{"x":0,"y":30,"width":1920,"height":1050},"nodes":[{"id":5,"type":"con","rect":{"x":0,"y":0,"width":100,"height":100},"nodes":[],"floating_nodes":[],"focus":[],"focused":false}],"floating_nodes":[],"focus":[5]}"##);
        let cmds = retile_commands(&ws, &[31, 32, 33], Some(31));
        assert_eq!(cmds, ["[con_id=31] floating disable", "[con_id=31] focus", "[con_id=31] split h", "[con_id=32] floating disable", "[con_id=32] focus", "[con_id=32] split v", "[con_id=33] floating disable", "[con_id=31] focus"]);
    }

    #[test]
    fn retile_splitv_workspace_matches_reference() {
        let ws = parse(r##"{"id":100,"type":"workspace","name":"100","layout":"splitv","rect":{"x":0,"y":30,"width":1920,"height":1050},"nodes":[{"id":5,"type":"con","rect":{"x":0,"y":0,"width":100,"height":100},"nodes":[],"floating_nodes":[],"focus":[],"focused":false},{"id":6,"type":"con","rect":{"x":0,"y":0,"width":100,"height":100},"nodes":[],"floating_nodes":[],"focus":[],"focused":false}],"floating_nodes":[],"focus":[5]}"##);
        let cmds = retile_commands(&ws, &[41, 42, 43, 44, 45], Some(41));
        assert_eq!(cmds, ["[con_id=41] floating disable", "[con_id=41] focus", "[con_id=41] split h", "[con_id=42] floating disable", "[con_id=42] focus", "[con_id=43] floating disable", "[con_id=43] focus", "[con_id=44] floating disable", "[con_id=44] focus", "[con_id=45] floating disable", "[con_id=41] focus"]);
    }

    #[test]
    fn retile_portrait_empty_matches_reference() {
        let ws = parse(r##"{"id":100,"type":"workspace","name":"100","layout":"splith","rect":{"x":0,"y":0,"width":1080,"height":1920},"nodes":[],"floating_nodes":[],"focus":[]}"##);
        let cmds = retile_commands(&ws, &[51, 52, 53], Some(53));
        assert_eq!(cmds, ["[con_id=51] floating disable", "[con_id=51] focus", "[con_id=51] split v", "[con_id=52] floating disable", "[con_id=52] focus", "[con_id=52] split h", "[con_id=53] floating disable", "[con_id=53] focus"]);
    }

    #[test]
    fn retile_single_window_matches_reference() {
        let ws = parse(r##"{"id":100,"type":"workspace","name":"100","layout":"splith","rect":{"x":0,"y":30,"width":1920,"height":1050},"nodes":[{"id":5,"type":"con","rect":{"x":0,"y":0,"width":100,"height":100},"nodes":[],"floating_nodes":[],"focus":[],"focused":false}],"floating_nodes":[],"focus":[5]}"##);
        let cmds = retile_commands(&ws, &[61], Some(61));
        assert_eq!(cmds, ["[con_id=61] floating disable", "[con_id=61] focus"]);
    }

    #[test]
    fn retile_no_windows_matches_reference() {
        let ws = parse(r##"{"id":100,"type":"workspace","name":"100","layout":"splith","rect":{"x":0,"y":30,"width":1920,"height":1050},"nodes":[{"id":5,"type":"con","rect":{"x":0,"y":0,"width":100,"height":100},"nodes":[],"floating_nodes":[],"focus":[],"focused":false}],"floating_nodes":[],"focus":[5]}"##);
        let cmds = retile_commands(&ws, &[], Some(5));
        assert_eq!(cmds, ["[con_id=5] focus"]);
    }

    #[test]
    fn focus_inactive_leaf_follows_focus_history() {
        let ws = parse(r##"{"id":100,"type":"workspace","name":"100","layout":"splith","rect":{"x":0,"y":30,"width":1920,"height":1050},"nodes":[{"id":5,"type":"con","rect":{"x":0,"y":0,"width":100,"height":100},"nodes":[],"floating_nodes":[],"focus":[],"focused":false},{"id":6,"type":"con","layout":"splitv","rect":{"x":0,"y":0,"width":100,"height":100},"nodes":[{"id":7,"type":"con","rect":{"x":0,"y":0,"width":100,"height":100},"nodes":[],"floating_nodes":[],"focus":[],"focused":false},{"id":8,"type":"con","rect":{"x":0,"y":0,"width":100,"height":100},"nodes":[],"floating_nodes":[],"focus":[],"focused":false}],"floating_nodes":[],"focus":[8,7]}],"floating_nodes":[],"focus":[6,5]}"##);
        assert_eq!(focus_inactive_leaf(&ws), Some(8));
    }

    #[test]
    fn focus_inactive_leaf_falls_back_to_last_leaf() {
        let ws = parse(r##"{"id":100,"type":"workspace","name":"100","layout":"splith","rect":{"x":0,"y":30,"width":1920,"height":1050},"nodes":[{"id":5,"type":"con","rect":{"x":0,"y":0,"width":100,"height":100},"nodes":[],"floating_nodes":[],"focus":[],"focused":false},{"id":9,"type":"con","rect":{"x":0,"y":0,"width":100,"height":100},"nodes":[],"floating_nodes":[],"focus":[],"focused":false}],"floating_nodes":[{"id":777,"type":"con","rect":{"x":0,"y":0,"width":100,"height":100},"nodes":[],"floating_nodes":[],"focus":[],"focused":false}],"focus":[777]}"##);
        assert_eq!(focus_inactive_leaf(&ws), Some(9));
    }

    #[test]
    fn focus_inactive_leaf_of_empty_workspace_is_none() {
        let ws = parse(r##"{"id":100,"type":"workspace","name":"100","layout":"splith","rect":{"x":0,"y":30,"width":1920,"height":1050},"nodes":[],"floating_nodes":[],"focus":[]}"##);
        assert_eq!(focus_inactive_leaf(&ws), None);
    }

    #[test]
    fn focus_inactive_leaf_without_focus_history_descends_to_first_child() {
        let ws = parse(r##"{"id":100,"type":"workspace","name":"100","layout":"splith","rect":{"x":0,"y":30,"width":1920,"height":1050},"nodes":[{"id":6,"type":"con","layout":"splitv","rect":{"x":0,"y":0,"width":100,"height":100},"nodes":[{"id":7,"type":"con","rect":{"x":0,"y":0,"width":100,"height":100},"nodes":[],"floating_nodes":[],"focus":[],"focused":false},{"id":8,"type":"con","rect":{"x":0,"y":0,"width":100,"height":100},"nodes":[],"floating_nodes":[],"focus":[],"focused":false}],"floating_nodes":[],"focus":[]}],"floating_nodes":[],"focus":[6]}"##);
        assert_eq!(focus_inactive_leaf(&ws), Some(7));
    }
}
