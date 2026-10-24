//! The two directions of the toggle (tiling -> floating, floating -> tiling),
//! the placement of windows that appear while a workspace is floating,
//! and the post-placement verification pass.

use std::collections::HashMap;

use serde_json::json;

use crate::error::{Error, Result};
use crate::geom::{
    close_rects, detect_gap, grid_layout, restore_rect, scan_layout, Rect, Spacing,
};
use crate::ipc::Sway;
use crate::sim::retile_commands;
use crate::state::{saved_geom, State, StateLock, WsState};
use crate::tree::{collect_leaves, locate_window, walk, Node};

/// Sort key for windows missing from the saved order (Python: `1 << 30`).
const UNORDERED: usize = 1 << 30;

/// `(con id, target rect)` pairs in insertion order.
pub type Placement = Vec<(i64, Rect)>;

fn lookup(placed: &[(i64, Rect)], id: i64) -> Option<Rect> {
    placed.iter().find(|(i, _)| *i == id).map(|&(_, r)| r)
}

/// Sway command that (optionally floats and) resizes/moves a window so that its
/// *outer* rect (including borders and title bar) equals `rect`.
pub fn place_command(node: &Node, rect: Rect, enable: bool) -> String {
    let r = node.rect;
    let wr = node.window_rect.unwrap_or(r);
    let dw = (r.width - wr.width).max(0);
    let dh = (r.height - wr.height).max(0);
    format!(
        "[con_id={}] {}resize set width {} px height {} px, move absolute position {} px {} px",
        node.id,
        if enable { "floating enable, " } else { "" },
        (rect.w - dw).max(1),
        (rect.h - dh).max(1),
        rect.x,
        rect.y
    )
}

/// Re-reads the tree and nudges every window whose real rect differs from the
/// target (Sway may adjust sizes for borders, min sizes, ...).
pub fn verify(sway: &mut Sway, targets: &[(i64, Rect)]) -> Result<()> {
    let tree = sway.tree()?;
    let index: HashMap<i64, &Node> = walk(&tree).map(|v| (v.node.id, v.node)).collect();
    let mut fixes = Vec::new();
    for &(wid, target) in targets {
        let Some(n) = index.get(&wid) else {
            continue;
        };
        let r = n.rect;
        let wr = n.window_rect.unwrap_or(r);
        let (dw, dh) = (target.w - r.width, target.h - r.height);
        let mut parts: Vec<String> = Vec::new();
        if (dw != 0 || dh != 0) && dw.abs() <= 48 && dh.abs() <= 48 {
            parts.push(format!(
                "resize set width {} px height {} px",
                (wr.width + dw).max(1),
                (wr.height + dh).max(1)
            ));
        }
        if !parts.is_empty() || r.x != target.x || r.y != target.y {
            parts.push(format!("move absolute position {} px {} px", target.x, target.y));
        }
        if !parts.is_empty() {
            fixes.push(format!("[con_id={}] {}", wid, parts.join(", ")));
        }
    }
    if !fixes.is_empty() {
        sway.run(&fixes.join("; "))?;
    }
    Ok(())
}

/// Tiling -> floating for the workspace `ws`.
///
/// Returns the ids of the windows that were floated and the rects assigned by the
/// automatic layout (windows restored from saved geometry are not part of the latter).
pub fn to_floating(
    sway: &mut Sway,
    state: &mut State,
    ws: &Node,
    tiled: &[&Node],
    floating: &[&Node],
    focused: Option<&Node>,
) -> Result<(Vec<i64>, Placement)> {
    let area = ws.bounds();
    let tiled_rects: Vec<Rect> = tiled.iter().map(|n| n.bounds()).collect();
    let gap = detect_gap(area, &tiled_rects).or(state.gap).filter(|&g| g != 0);
    if let Some(g) = gap {
        state.gap = Some(g);
    }
    let sp = gap.map_or(Spacing::DEFAULT, Spacing::uniform);

    let mut placed: Placement = Vec::new();
    let mut fresh: Vec<&Node> = Vec::new();
    for &n in tiled {
        match state.geom.get(&n.id.to_string()).and_then(saved_geom) {
            Some(saved) => placed.push((n.id, restore_rect(&saved, area))),
            None => fresh.push(n),
        }
    }

    let mut occupied: Vec<Rect> = floating.iter().map(|n| n.bounds()).collect();
    occupied.extend(placed.iter().map(|&(_, r)| r));

    let mut auto: Placement = Vec::new();
    if !fresh.is_empty() {
        let rects = if occupied.is_empty() {
            grid_layout(fresh.len(), area, sp)
        } else {
            scan_layout(fresh.len(), area, &occupied, tiled.len() + floating.len(), sp)
        };
        for (n, r) in fresh.iter().zip(rects) {
            placed.push((n.id, r));
            auto.push((n.id, r));
        }
    }

    if !tiled.is_empty() {
        let mut cmds: Vec<String> = tiled
            .iter()
            .filter_map(|n| lookup(&placed, n.id).map(|r| place_command(n, r, true)))
            .collect();
        if let Some(f) = focused {
            if lookup(&placed, f.id).is_some() {
                cmds.push(format!("[con_id={}] focus", f.id));
            }
        }
        sway.run(&cmds.join("; "))?;
        verify(sway, &placed)?;
    }
    Ok((tiled.iter().map(|n| n.id).collect(), auto))
}

/// Floating -> tiling: remembers every managed window's geometry and re-inserts
/// the windows into the tiling tree in their original order.
pub fn to_tiling(
    sway: &mut Sway,
    state: &mut State,
    ws: &Node,
    ws_state: &WsState,
    floating: &[&Node],
    managed: &[i64],
    focused: Option<&Node>,
) -> Result<()> {
    let area = ws.bounds();
    let by_id: HashMap<i64, &Node> = floating.iter().map(|&n| (n.id, n)).collect();
    for wid in managed {
        let Some(n) = by_id.get(wid) else {
            continue;
        };
        let r = n.bounds();
        state.geom.insert(
            wid.to_string(),
            json!({"r": [r.x - area.x, r.y - area.y, r.w, r.h], "a": [area.w, area.h]}),
        );
    }

    let order: HashMap<i64, usize> = ws_state
        .order
        .iter()
        .enumerate()
        .map(|(i, &id)| (id, i))
        .collect();
    let mut ordered = managed.to_vec();
    ordered.sort_by_key(|&id| (order.get(&id).copied().unwrap_or(UNORDERED), id));
    if !ordered.is_empty() {
        let cmds = retile_commands(ws, &ordered, focused.map(|f| f.id));
        sway.run(&cmds.join("; "))?;
    }
    Ok(())
}

/// Daemon handler: a window appeared (or moved) on a workspace in floating mode —
/// float it and re-flow the windows that still sit where the auto-layout put them.
pub fn float_new_window(sway: &mut Sway, con_id: i64) -> Result<()> {
    let _lock = StateLock::acquire()?;
    let mut state = State::load(sway.path());
    if !state.any_floating() {
        return Ok(());
    }
    let tree = sway.tree()?;
    let Some((ws, node)) = locate_window(&tree, con_id) else {
        return Ok(());
    };
    let ws_name = ws.name.as_deref().unwrap_or("");
    let Some(ws_state) = state
        .ws
        .get(ws_name)
        .filter(|s| s.is_floating_mode())
        .cloned()
    else {
        return Ok(());
    };
    if node.kind != "con" || !node.nodes.is_empty() || node.is_fullscreen() {
        return Ok(());
    }

    let sp = state.spacing();
    let area = ws.bounds();
    let floating = collect_leaves(&ws.floating_nodes);

    let mut auto: HashMap<i64, Rect> = HashMap::new();
    for (key, value) in &ws_state.auto {
        let id: i64 = key
            .parse()
            .map_err(|_| Error::Protocol(format!("invalid window id in state file: {key:?}")))?;
        auto.insert(id, Rect::from_array(*value));
    }
    let order: HashMap<i64, usize> = ws_state
        .order
        .iter()
        .enumerate()
        .map(|(i, &id)| (id, i))
        .collect();

    // Windows still at their auto-placed rect may be moved again; the rest are obstacles.
    let mut movable: Vec<&Node> = Vec::new();
    let mut fixed: Vec<Rect> = Vec::new();
    for &n in &floating {
        match auto.get(&n.id) {
            Some(&a) if close_rects(n.bounds(), a) => movable.push(n),
            _ => fixed.push(n.bounds()),
        }
    }
    movable.sort_by_key(|n| (order.get(&n.id).copied().unwrap_or(UNORDERED), n.id));

    let mut batch = movable;
    batch.push(node);
    let rects = if fixed.is_empty() {
        grid_layout(batch.len(), area, sp)
    } else {
        scan_layout(batch.len(), area, &fixed, floating.len() + 1, sp)
    };
    let placed: Placement = batch.iter().zip(&rects).map(|(n, &r)| (n.id, r)).collect();
    let last = batch.len() - 1;
    let cmds: Vec<String> = batch
        .iter()
        .zip(&rects)
        .enumerate()
        .map(|(i, (n, &r))| place_command(n, r, i == last))
        .collect();
    sway.run(&cmds.join("; "))?;
    verify(sway, &placed)?;

    if let Some(entry) = state.ws.get_mut(ws_name) {
        entry.managed.push(con_id);
        entry.auto = placed
            .iter()
            .map(|(id, r)| (id.to_string(), r.to_array()))
            .collect();
    }
    state.save()
}

/// Daemon handler: a workspace became empty — drop its floating-mode state.
pub fn forget_workspace(sock_path: &str, name: &str) -> Result<()> {
    if name.is_empty() {
        return Ok(());
    }
    let _lock = StateLock::acquire()?;
    let mut state = State::load(sock_path);
    if state.ws.remove(name).is_some() {
        state.save()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected values below were produced by the reference Python implementation (swayflow_ver4.py).

    #[test]
    fn place_command_case_0_matches_reference() {
        let node: Node = serde_json::from_str(r##"{"id":7,"rect":{"x":0,"y":0,"width":800,"height":600},"window_rect":{"x":2,"y":24,"width":796,"height":574}}"##).expect("valid node");
        let cmd = place_command(&node, Rect::new(100, 120, 640, 480), true);
        assert_eq!(cmd, "[con_id=7] floating enable, resize set width 636 px height 454 px, move absolute position 100 px 120 px");
    }

    #[test]
    fn place_command_case_1_matches_reference() {
        let node: Node = serde_json::from_str(r##"{"id":7,"rect":{"x":0,"y":0,"width":800,"height":600},"window_rect":{"x":2,"y":24,"width":796,"height":574}}"##).expect("valid node");
        let cmd = place_command(&node, Rect::new(100, 120, 640, 480), false);
        assert_eq!(cmd, "[con_id=7] resize set width 636 px height 454 px, move absolute position 100 px 120 px");
    }

    #[test]
    fn place_command_case_2_matches_reference() {
        let node: Node = serde_json::from_str(r##"{"id":8,"rect":{"x":0,"y":0,"width":800,"height":600}}"##).expect("valid node");
        let cmd = place_command(&node, Rect::new(5, 6, 1, 1), true);
        assert_eq!(cmd, "[con_id=8] floating enable, resize set width 1 px height 1 px, move absolute position 5 px 6 px");
    }

    #[test]
    fn place_command_case_3_matches_reference() {
        let node: Node = serde_json::from_str(r##"{"id":9,"rect":{"x":0,"y":0,"width":800,"height":600},"window_rect":{"x":0,"y":0,"width":900,"height":700}}"##).expect("valid node");
        let cmd = place_command(&node, Rect::new(0, 0, 10, 10), false);
        assert_eq!(cmd, "[con_id=9] resize set width 10 px height 10 px, move absolute position 0 px 0 px");
    }

    #[test]
    fn place_command_case_4_matches_reference() {
        let node: Node = serde_json::from_str(r##"{"id":10,"rect":{"x":0,"y":0,"width":800,"height":600},"window_rect":{"x":2,"y":2,"width":796,"height":596}}"##).expect("valid node");
        let cmd = place_command(&node, Rect::new(10, 10, 3, 3), true);
        assert_eq!(cmd, "[con_id=10] floating enable, resize set width 1 px height 1 px, move absolute position 10 px 10 px");
    }

    #[test]
    fn lookup_finds_by_id() {
        let placed = vec![(1, Rect::new(0, 0, 1, 1)), (2, Rect::new(5, 5, 2, 2))];
        assert_eq!(lookup(&placed, 2), Some(Rect::new(5, 5, 2, 2)));
        assert_eq!(lookup(&placed, 3), None);
    }
}
