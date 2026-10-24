//! `toggle` and `daemon` commands.

use std::collections::HashSet;
use std::env;

use serde_json::Value;

use crate::error::{Error, Result};
use crate::floating::{float_new_window, forget_workspace, to_floating, to_tiling};
use crate::ipc::{Sway, EVENT_WINDOW, EVENT_WORKSPACE};
use crate::paths::paths;
use crate::state::{parse_id, State, StateLock, WsState};
use crate::sys;
use crate::tree::{collect_leaves, locate_focus, walk, Node};

/// Starts the event daemon unless one is already running (it holds the daemon lock).
fn ensure_daemon() -> Result<()> {
    match sys::try_lock_exclusive(&paths().daemon_lock)? {
        Some(probe) => drop(probe),
        None => return Ok(()),
    }
    let exe = env::current_exe()?;
    sys::spawn_detached(&exe, "daemon")?;
    Ok(())
}

/// Toggles the focused workspace between tiling and floating (auto-arranged) mode.
pub fn cmd_toggle() -> Result<u8> {
    let _lock = StateLock::acquire()?;
    let mut sway = Sway::connect()?;
    let tree = sway.tree()?;
    let Some(focus) = locate_focus(&tree) else {
        return Ok(0);
    };
    let ws = focus.workspace;
    let focused = focus.leaf;

    let mut state = State::load(sway.path());
    let live: HashSet<i64> = walk(&tree).map(|v| v.node.id).collect();
    let names: HashSet<&str> = walk(&tree)
        .filter(|v| v.node.is_workspace())
        .map(|v| v.node.name.as_deref().unwrap_or(""))
        .collect();
    // Forget geometry of windows and state of workspaces that no longer exist.
    state
        .geom
        .retain(|k, _| parse_id(k).is_some_and(|id| live.contains(&id)));
    state.ws.retain(|k, _| names.contains(k.as_str()));

    let tiled: Vec<&Node> = collect_leaves(&ws.nodes)
        .into_iter()
        .filter(|n| !n.is_fullscreen())
        .collect();
    let floating = collect_leaves(&ws.floating_nodes);
    let floating_ids: HashSet<i64> = floating.iter().map(|n| n.id).collect();
    let name = ws.name.as_deref().unwrap_or("");
    let ws_state = state
        .ws
        .get(name)
        .filter(|s| s.is_floating_mode())
        .cloned();

    if let Some(ws_state) = ws_state {
        let managed: Vec<i64> = ws_state
            .managed
            .iter()
            .copied()
            .filter(|id| floating_ids.contains(id))
            .collect();
        to_tiling(&mut sway, &mut state, ws, &ws_state, &floating, &managed, focused)?;
        state.ws.remove(name);
    } else {
        let (ids, auto) = to_floating(&mut sway, &mut state, ws, &tiled, &floating, focused)?;
        state.ws.insert(
            name.to_owned(),
            WsState {
                floating: Some(true),
                managed: ids,
                order: tiled.iter().map(|n| n.id).collect(),
                auto: auto
                    .iter()
                    .map(|(id, r)| (id.to_string(), r.to_array()))
                    .collect(),
            },
        );
        ensure_daemon()?;
    }
    state.save()?;
    Ok(0)
}

/// Reacts to one Sway event.
fn handle_event(worker: &mut Sway, mtype: u32, payload: &Value) -> Result<()> {
    let change = payload.get("change").and_then(Value::as_str);
    match (mtype, change) {
        (EVENT_WINDOW, Some("new" | "move")) => {
            let Some(con) = payload.get("container") else {
                return Ok(());
            };
            let is_con = con.get("type").and_then(Value::as_str) == Some("con");
            let has_kids = con
                .get("nodes")
                .and_then(Value::as_array)
                .is_some_and(|nodes| !nodes.is_empty());
            if is_con && !has_kids {
                let id = con
                    .get("id")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| Error::Protocol("window event without a container id".into()))?;
                float_new_window(worker, id)?;
            }
            Ok(())
        }
        (EVENT_WORKSPACE, Some("empty")) => {
            let name = payload
                .pointer("/current/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            forget_workspace(worker.path(), name)
        }
        _ => Ok(()),
    }
}

/// Long-running process: keeps new windows of floating workspaces auto-arranged.
/// Exits quietly when Sway goes away. Only one instance can run at a time.
pub fn cmd_daemon() -> Result<u8> {
    let Some(_daemon_lock) = sys::try_lock_exclusive(&paths().daemon_lock)? else {
        return Ok(0);
    };
    let mut events = Sway::connect()?;
    let mut worker = Sway::connect()?;
    events.subscribe(&["window", "workspace"])?;
    loop {
        let (mtype, payload) = match events.read_message() {
            Ok(message) => message,
            Err(Error::Disconnected(_)) => return Ok(0),
            Err(err) => return Err(err),
        };
        match handle_event(&mut worker, mtype, &payload) {
            Ok(()) => {}
            Err(Error::Disconnected(_)) => return Ok(0),
            Err(err) => eprintln!("swayflow: {err}"),
        }
    }
}
