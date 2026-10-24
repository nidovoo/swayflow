//! Persistent state shared between the CLI and the daemon.
//!
//! The JSON layout is identical to the Python original:
//!
//! ```json
//! {"sock": "...", "gap": 10,
//!  "geom": {"<con_id>": {"r": [x, y, w, h], "a": [ws_w, ws_h]}},
//!  "ws":   {"<workspace>": {"floating": true, "managed": [ids], "order": [ids],
//!                           "auto": {"<con_id>": [x, y, w, h]}}}}
//! ```

use std::collections::BTreeMap;
use std::fs;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::geom::{SavedGeom, Spacing};
use crate::paths::paths;
use crate::sys::{self, FileLock};

/// Per-workspace state of the floating mode.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WsState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floating: Option<bool>,
    /// Windows put under swayflow's control (duplicates are possible, as in the original).
    #[serde(default)]
    pub managed: Vec<i64>,
    /// Tiling order at the moment floating mode was entered.
    #[serde(default)]
    pub order: Vec<i64>,
    /// Rects assigned by the last auto-placement, keyed by con id.
    #[serde(default)]
    pub auto: BTreeMap<String, [i64; 4]>,
}

impl WsState {
    /// Python `is_floating_mode`: `bool(ws.get("floating", ws.get("managed")))`.
    pub fn is_floating_mode(&self) -> bool {
        self.floating.unwrap_or(!self.managed.is_empty())
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct State {
    pub sock: String,
    /// Kept as raw JSON so that one malformed entry only invalidates itself.
    pub geom: BTreeMap<String, Value>,
    pub ws: BTreeMap<String, WsState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<i64>,
}

impl State {
    fn fresh(sock: &str) -> State {
        State {
            sock: sock.to_owned(),
            geom: BTreeMap::new(),
            ws: BTreeMap::new(),
            gap: None,
        }
    }

    /// Loads the state for the given Sway socket; a missing, unreadable, malformed
    /// or foreign (other Sway instance) file yields a fresh state.
    pub fn load(sock: &str) -> State {
        fs::read(&paths().state)
            .ok()
            .and_then(|raw| serde_json::from_slice::<State>(&raw).ok())
            .filter(|st| st.sock == sock)
            .unwrap_or_else(|| State::fresh(sock))
    }

    /// Atomic write: temporary file + rename.
    pub fn save(&self) -> Result<()> {
        let p = paths();
        fs::write(&p.state_tmp, serde_json::to_vec(self)?)?;
        fs::rename(&p.state_tmp, &p.state)?;
        Ok(())
    }

    pub fn any_floating(&self) -> bool {
        self.ws.values().any(WsState::is_floating_mode)
    }

    /// The remembered compositor gap (a stored `0` counts as "none", like Python's truthiness).
    pub fn gap(&self) -> Option<i64> {
        self.gap.filter(|&g| g != 0)
    }

    /// Spacing to use for layouts: the remembered gap, or the built-in defaults.
    pub fn spacing(&self) -> Spacing {
        self.gap().map_or(Spacing::DEFAULT, Spacing::uniform)
    }
}

/// Parses a `geom` entry; anything malformed is treated as "no saved geometry".
pub fn saved_geom(value: &Value) -> Option<SavedGeom> {
    #[derive(Deserialize)]
    struct Raw {
        r: [f64; 4],
        a: [f64; 2],
    }
    let raw = Raw::deserialize(value).ok()?;
    Some(SavedGeom {
        rect: raw.r,
        area: raw.a,
    })
}

/// Python `k.isdigit() and int(k)`: a key that is a plain decimal window id.
pub fn parse_id(key: &str) -> Option<i64> {
    if key.is_empty() || !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    key.parse().ok()
}

/// Global exclusive lock around every read-modify-write of the state file.
pub struct StateLock {
    _lock: FileLock,
}

impl StateLock {
    pub fn acquire() -> Result<StateLock> {
        Ok(StateLock {
            _lock: sys::lock_exclusive(&paths().lock)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A state file in exactly the shape the Python implementation writes.
    const SAMPLE: &str = r#"{"sock":"/run/user/1000/sway-ipc.sock","gap":10,"geom":{"42":{"r":[10,20,300,200],"a":[1920,1080]},"43":{"r":[1,2]}},"ws":{"1":{"floating":true,"managed":[1,2],"order":[2,1],"auto":{"1":[0,0,10,10]}},"legacy":{"managed":[3]},"off":{"floating":false,"managed":[3]}}}"#;

    fn sample() -> State {
        serde_json::from_str(SAMPLE).expect("sample state parses")
    }

    #[test]
    fn parses_state_written_by_the_python_version() {
        let st = sample();
        assert_eq!(st.sock, "/run/user/1000/sway-ipc.sock");
        assert_eq!(st.gap(), Some(10));
        assert_eq!(st.spacing(), Spacing::uniform(10));
        assert!(st.any_floating());
        let ws = &st.ws["1"];
        assert_eq!(ws.managed, [1, 2]);
        assert_eq!(ws.order, [2, 1]);
        assert_eq!(ws.auto["1"], [0, 0, 10, 10]);
    }

    #[test]
    fn floating_mode_follows_the_python_truthiness_rules() {
        let st = sample();
        assert!(st.ws["1"].is_floating_mode());
        assert!(st.ws["legacy"].is_floating_mode(), "legacy entry: non-empty `managed`");
        assert!(!st.ws["off"].is_floating_mode(), "explicit `floating: false` wins");
        assert!(!WsState::default().is_floating_mode());
    }

    #[test]
    fn malformed_geom_entries_are_ignored_individually() {
        let st = sample();
        let saved = saved_geom(&st.geom["42"]).expect("valid entry");
        assert_eq!(saved.rect, [10.0, 20.0, 300.0, 200.0]);
        assert_eq!(saved.area, [1920.0, 1080.0]);
        assert!(saved_geom(&st.geom["43"]).is_none());
        assert!(saved_geom(&serde_json::json!({})).is_none());
        assert!(saved_geom(&serde_json::json!(null)).is_none());
    }

    #[test]
    fn zero_gap_means_no_gap() {
        let mut st = sample();
        st.gap = Some(0);
        assert_eq!(st.gap(), None);
        assert_eq!(st.spacing(), Spacing::DEFAULT);
        st.gap = None;
        assert_eq!(st.spacing(), Spacing::DEFAULT);
    }

    #[test]
    fn serialization_keeps_the_file_layout() {
        let mut st = sample();
        st.gap = None;
        let value = serde_json::to_value(&st).expect("serializes");
        assert!(value.get("gap").is_none());
        assert_eq!(value["ws"]["1"]["floating"], serde_json::json!(true));
        assert!(value["ws"]["legacy"].get("floating").is_none());
        assert_eq!(value["ws"]["1"]["auto"]["1"], serde_json::json!([0, 0, 10, 10]));
        assert_eq!(value["geom"]["42"]["r"], serde_json::json!([10, 20, 300, 200]));
    }

    #[test]
    fn state_without_required_sections_is_rejected() {
        assert!(serde_json::from_str::<State>(r#"{"sock":"x"}"#).is_err());
        assert!(serde_json::from_str::<State>(r#"{"geom":{},"ws":{}}"#).is_err());
    }

    #[test]
    fn parse_id_accepts_only_plain_decimal_ids() {
        assert_eq!(parse_id("123"), Some(123));
        assert_eq!(parse_id("0"), Some(0));
        assert_eq!(parse_id(""), None);
        assert_eq!(parse_id("-5"), None);
        assert_eq!(parse_id("12a"), None);
        assert_eq!(parse_id("+7"), None);
    }
}
