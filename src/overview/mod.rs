//! The `overview` command: a full-screen layer-shell overlay with a thumbnail
//! of every workspace of the focused output.
//!
//! This module holds the data collection, the card layout maths and the
//! pid-file handling; the GTK front-end lives in `ui` (feature `overview`).

#[cfg(feature = "overview")]
mod ui;

use std::env;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::error::Result;
use crate::geom::Rect;
use crate::ipc::Sway;
use crate::paths::paths;
use crate::sys;
use crate::tree::{collect_leaves, locate_focus, Node};

pub struct WindowInfo {
    pub id: i64,
    /// Position relative to the output.
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
    /// `app_id` or X11 class — used for the icon lookup.
    pub key: String,
    pub floating: bool,
    pub focused: bool,
}

pub struct WorkspaceInfo {
    pub name: String,
    pub num: i64,
    pub visible: bool,
    pub wins: Vec<WindowInfo>,
}

pub struct OverviewData {
    #[allow(dead_code)]
    pub output: String,
    /// Output rectangle in global layout coordinates.
    pub rect: Rect,
    pub workspaces: Vec<WorkspaceInfo>,
    /// Index of the workspace currently shown on the output.
    pub current: Option<usize>,
}

/// Collects the workspaces and windows of the output that holds the focus.
pub fn collect_overview(tree: &Node) -> Option<OverviewData> {
    let out = locate_focus(tree)?.output;
    let (ox, oy, ow, oh) = (out.rect.x, out.rect.y, out.rect.width, out.rect.height);
    if ow <= 0 || oh <= 0 {
        return None;
    }

    let mut current: Option<String> = out.current_workspace.clone();
    if current.is_none() {
        if let Some(&first) = out.focus.first() {
            for ws in &out.nodes {
                if ws.id == first {
                    current = ws.name.clone();
                }
            }
        }
    }

    let mut workspaces = Vec::new();
    for ws in out.nodes.iter().filter(|n| n.is_workspace()) {
        let mut wins = Vec::new();
        for (floating, nodes) in [(false, &ws.nodes), (true, &ws.floating_nodes)] {
            for n in collect_leaves(nodes) {
                let r = n.rect;
                if r.width <= 0 || r.height <= 0 {
                    continue;
                }
                wins.push(WindowInfo {
                    id: n.id,
                    x: r.x - ox,
                    y: r.y - oy,
                    w: r.width,
                    h: r.height,
                    key: n.window_key().to_owned(),
                    floating,
                    focused: n.focused,
                });
            }
        }
        let name = ws.name.clone().unwrap_or_default();
        workspaces.push(WorkspaceInfo {
            visible: current.as_deref() == Some(name.as_str()),
            num: ws.num.unwrap_or(-1),
            name,
            wins,
        });
    }
    if workspaces.is_empty() {
        return None;
    }
    let index = workspaces.iter().position(|w| w.visible);
    Some(OverviewData {
        output: out.name.clone().unwrap_or_default(),
        rect: Rect::new(ox, oy, ow, oh),
        workspaces,
        current: index,
    })
}

#[derive(Clone, Copy, Debug)]
pub struct CardRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Lays `n` cards of the given aspect ratio out in a centred grid inside
/// `width`×`height`; picks the column count that gives the widest cards.
/// Returns the card rects and the chosen number of columns.
pub fn card_layout(n: usize, width: f64, height: f64, aspect: f64) -> (Vec<CardRect>, usize) {
    let (px, py) = (width * 0.06, height * 0.06);
    let (aw, ah) = (width - 2.0 * px, height - 2.0 * py);
    let gap = (width * 0.018).max(12.0);

    // (card width, card height, columns, rows)
    let mut best: Option<(f64, f64, usize, usize)> = None;
    for cols in 1..=n {
        let rows = n.div_ceil(cols);
        let (colsf, rowsf) = (cols as f64, rows as f64);
        let mut cw = ((aw - (colsf - 1.0) * gap) / colsf).min(width * 0.46);
        let mut ch = cw / aspect;
        if rowsf * ch + (rowsf - 1.0) * gap > ah {
            ch = (ah - (rowsf - 1.0) * gap) / rowsf;
            cw = ch * aspect;
        }
        if cw <= 0.0 {
            continue;
        }
        if best.map_or(true, |b| cw > b.0 + 1e-6) {
            best = Some((cw, ch, cols, rows));
        }
    }
    let (cw, ch, cols, rows) = best.unwrap_or((width * 0.4, height * 0.4, 1, n));
    let total_h = rows as f64 * ch + (rows as f64 - 1.0) * gap;

    let rects = (0..n)
        .map(|i| {
            let (r, c) = (i / cols, i % cols);
            let cnt = if r < rows.saturating_sub(1) {
                cols
            } else {
                n - cols * rows.saturating_sub(1)
            };
            let cntf = cnt as f64;
            let row_w = cntf * cw + (cntf - 1.0) * gap;
            CardRect {
                x: (width - row_w) / 2.0 + c as f64 * (cw + gap),
                y: (height - total_h) / 2.0 + r as f64 * (ch + gap),
                w: cw,
                h: ch,
            }
        })
        .collect();
    (rects, cols)
}

/// Quotes a string for use as a Sway command argument.
pub fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || haystack.windows(needle.len()).any(|w| w == needle)
}

/// If another overview is already open, asks it to close and returns `true`
/// (so that running `swayflow overview` twice toggles the overlay).
fn overview_running() -> bool {
    let Some(pid) = fs::read_to_string(&paths().pid)
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
    else {
        return false;
    };
    if u32::try_from(pid).is_ok_and(|p| p == std::process::id()) {
        return false;
    }
    let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let me: Vec<u8> = env::args_os()
        .next()
        .and_then(|arg0| Path::new(&arg0).file_name().map(|n| n.as_bytes().to_vec()))
        .unwrap_or_default();
    if contains(&cmdline, &me) && contains(&cmdline, b"overview") {
        return sys::terminate(pid).is_ok();
    }
    false
}

/// Pid file of the running overview; removed on drop.
struct PidFile;

impl PidFile {
    fn create() -> Result<PidFile> {
        fs::write(&paths().pid, std::process::id().to_string())?;
        Ok(PidFile)
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&paths().pid);
    }
}

#[cfg(feature = "overview")]
fn run_ui(data: OverviewData) -> Result<Option<String>> {
    ui::run_overview(data)
}

#[cfg(not(feature = "overview"))]
fn run_ui(_data: OverviewData) -> Result<Option<String>> {
    Err(crate::error::Error::runtime("built without the `overview` feature"))
}

pub fn cmd_overview(_args: &[String]) -> Result<u8> {
    if overview_running() {
        return Ok(0);
    }
    let tree = Sway::connect()?.tree()?;
    let Some(data) = collect_overview(&tree) else {
        return Ok(1);
    };
    let command = {
        let _pid_file = PidFile::create()?;
        run_ui(data)?
    };
    if let Some(command) = command {
        Sway::connect()?.run(&command)?;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected values below were produced by the reference Python implementation (swayflow_ver4.py).

    fn summarize(data: &OverviewData) -> Vec<String> {
        data.workspaces
            .iter()
            .map(|w| {
                let wins: Vec<String> = w
                    .wins
                    .iter()
                    .map(|x| {
                        format!(
                            "{},{},{},{},{},{},{},{}",
                            x.id, x.x, x.y, x.w, x.h, x.key, x.floating, x.focused
                        )
                    })
                    .collect();
                format!("{}|{}|{}|{}", w.name, w.num, w.visible, wins.join(";"))
            })
            .collect()
    }

    #[test]
    fn collect_overview_with_current_workspace_matches_reference() {
        let tree: Node = serde_json::from_str(r##"{"id":1,"type":"root","name":"root","rect":{"x":0,"y":0,"width":4480,"height":1440},"nodes":[{"id":2,"type":"output","name":"DP-1","rect":{"x":1920,"y":0,"width":2560,"height":1440},"nodes":[{"id":3,"type":"workspace","name":"1","num":1,"layout":"splith","rect":{"x":1920,"y":0,"width":2560,"height":1440},"nodes":[{"id":10,"type":"con","rect":{"x":1920,"y":0,"width":1280,"height":1440},"nodes":[],"floating_nodes":[],"focused":true,"app_id":"firefox"},{"id":11,"type":"con","rect":{"x":3200,"y":0,"width":1280,"height":1440},"nodes":[],"floating_nodes":[],"focused":false,"app_id":"","window_properties":{"class":"Steam"}},{"id":13,"type":"con","rect":{"x":3200,"y":0,"width":0,"height":0},"nodes":[],"floating_nodes":[],"focused":false,"app_id":"ghost"}],"floating_nodes":[{"id":12,"type":"floating_con","rect":{"x":2100,"y":200,"width":800,"height":600},"nodes":[],"floating_nodes":[],"focused":false,"app_id":"foot"}],"focus":[10,11]},{"id":4,"type":"workspace","name":"web","num":-1,"layout":"splith","rect":{"x":1920,"y":0,"width":2560,"height":1440},"nodes":[{"id":20,"type":"con","layout":"splitv","rect":{"x":1920,"y":0,"width":2560,"height":1440},"nodes":[{"id":21,"type":"con","rect":{"x":1920,"y":0,"width":2560,"height":720},"nodes":[],"floating_nodes":[],"focused":false,"app_id":null},{"id":22,"type":"con","rect":{"x":1920,"y":720,"width":2560,"height":720},"nodes":[],"floating_nodes":[],"focused":false,"app_id":"mpv"}],"floating_nodes":[],"focus":[22,21]}],"floating_nodes":[],"focus":[20]},{"id":5,"type":"workspace","name":"3","num":3,"layout":"splith","rect":{"x":1920,"y":0,"width":2560,"height":1440},"nodes":[],"floating_nodes":[],"focus":[]}],"floating_nodes":[],"focus":[4,3,5],"current_workspace":"1"}],"floating_nodes":[],"focus":[2]}"##).expect("valid tree");
        let data = collect_overview(&tree).expect("overview data");
        assert_eq!(data.rect, Rect::new(1920, 0, 2560, 1440));
        assert_eq!(data.current, Some(0));
        assert_eq!(summarize(&data), vec!["1|1|true|10,0,0,1280,1440,firefox,false,true;11,1280,0,1280,1440,Steam,false,false;12,180,200,800,600,foot,true,false", "web|-1|false|21,0,0,2560,720,?,false,false;22,0,720,2560,720,mpv,false,false", "3|3|false|"]);
    }

    #[test]
    fn collect_overview_current_from_focus_list_matches_reference() {
        let tree: Node = serde_json::from_str(r##"{"id":1,"type":"root","name":"root","rect":{"x":0,"y":0,"width":4480,"height":1440},"nodes":[{"id":2,"type":"output","name":"DP-1","rect":{"x":1920,"y":0,"width":2560,"height":1440},"nodes":[{"id":3,"type":"workspace","name":"1","num":1,"layout":"splith","rect":{"x":1920,"y":0,"width":2560,"height":1440},"nodes":[{"id":10,"type":"con","rect":{"x":1920,"y":0,"width":1280,"height":1440},"nodes":[],"floating_nodes":[],"focused":true,"app_id":"firefox"},{"id":11,"type":"con","rect":{"x":3200,"y":0,"width":1280,"height":1440},"nodes":[],"floating_nodes":[],"focused":false,"app_id":"","window_properties":{"class":"Steam"}},{"id":13,"type":"con","rect":{"x":3200,"y":0,"width":0,"height":0},"nodes":[],"floating_nodes":[],"focused":false,"app_id":"ghost"}],"floating_nodes":[{"id":12,"type":"floating_con","rect":{"x":2100,"y":200,"width":800,"height":600},"nodes":[],"floating_nodes":[],"focused":false,"app_id":"foot"}],"focus":[10,11]},{"id":4,"type":"workspace","name":"web","num":-1,"layout":"splith","rect":{"x":1920,"y":0,"width":2560,"height":1440},"nodes":[{"id":20,"type":"con","layout":"splitv","rect":{"x":1920,"y":0,"width":2560,"height":1440},"nodes":[{"id":21,"type":"con","rect":{"x":1920,"y":0,"width":2560,"height":720},"nodes":[],"floating_nodes":[],"focused":false,"app_id":null},{"id":22,"type":"con","rect":{"x":1920,"y":720,"width":2560,"height":720},"nodes":[],"floating_nodes":[],"focused":false,"app_id":"mpv"}],"floating_nodes":[],"focus":[22,21]}],"floating_nodes":[],"focus":[20]},{"id":5,"type":"workspace","name":"3","num":3,"layout":"splith","rect":{"x":1920,"y":0,"width":2560,"height":1440},"nodes":[],"floating_nodes":[],"focus":[]}],"floating_nodes":[],"focus":[4,3,5]}],"floating_nodes":[],"focus":[2]}"##).expect("valid tree");
        let data = collect_overview(&tree).expect("overview data");
        assert_eq!(data.rect, Rect::new(1920, 0, 2560, 1440));
        assert_eq!(data.current, Some(1));
        assert_eq!(summarize(&data), vec!["1|1|false|10,0,0,1280,1440,firefox,false,true;11,1280,0,1280,1440,Steam,false,false;12,180,200,800,600,foot,true,false", "web|-1|true|21,0,0,2560,720,?,false,false;22,0,720,2560,720,mpv,false,false", "3|3|false|"]);
    }

    #[test]
    fn card_layout_matches_reference() {
        type Case = (usize, f64, f64, f64, usize, Vec<(f64, f64, f64, f64)>);
        let cases: Vec<Case> = vec![
        (1, 1920.0, 1080.0, 1.7777777777777777, 1, vec![(518.4, 291.59999999999997, 883.2, 496.80000000000007)]),
        (2, 1920.0, 1080.0, 1.7777777777777777, 2, vec![(115.20000000000005, 307.26, 827.52, 465.48), (977.28, 307.26, 827.52, 465.48)]),
        (3, 1920.0, 1080.0, 1.7777777777777777, 2, vec![(128.6400000000001, 64.80000000000001, 814.0799999999999, 457.92), (977.28, 64.80000000000001, 814.0799999999999, 457.92), (552.96, 557.28, 814.0799999999999, 457.92)]),
        (4, 1920.0, 1080.0, 1.7777777777777777, 2, vec![(128.6400000000001, 64.80000000000001, 814.0799999999999, 457.92), (977.28, 64.80000000000001, 814.0799999999999, 457.92), (128.6400000000001, 557.28, 814.0799999999999, 457.92), (977.28, 557.28, 814.0799999999999, 457.92)]),
        (5, 1920.0, 1080.0, 1.7777777777777777, 3, vec![(115.20000000000005, 218.88000000000005, 540.16, 303.84), (689.92, 218.88000000000005, 540.16, 303.84), (1264.6399999999999, 218.88000000000005, 540.16, 303.84), (402.56000000000006, 557.28, 540.16, 303.84), (977.28, 557.28, 540.16, 303.84)]),
        (6, 1920.0, 1080.0, 1.7777777777777777, 3, vec![(115.20000000000005, 218.88000000000005, 540.16, 303.84), (689.92, 218.88000000000005, 540.16, 303.84), (1264.6399999999999, 218.88000000000005, 540.16, 303.84), (115.20000000000005, 557.28, 540.16, 303.84), (689.92, 557.28, 540.16, 303.84), (1264.6399999999999, 557.28, 540.16, 303.84)]),
        (7, 1920.0, 1080.0, 1.7777777777777777, 3, vec![(142.08000000000004, 64.80000000000001, 522.24, 293.76), (698.88, 64.80000000000001, 522.24, 293.76), (1255.6799999999998, 64.80000000000001, 522.24, 293.76), (142.08000000000004, 393.12, 522.24, 293.76), (698.88, 393.12, 522.24, 293.76), (1255.6799999999998, 393.12, 522.24, 293.76), (698.88, 721.44, 522.24, 293.76)]),
        (3, 1080.0, 1920.0, 0.5625, 2, vec![(64.79999999999995, 122.75999999999999, 465.48, 827.52), (549.72, 122.75999999999999, 465.48, 827.52), (307.26, 969.72, 465.48, 827.52)]),
        (5, 2560.0, 1440.0, 1.7777777777777777, 3, vec![(153.5999999999999, 291.83999999999986, 720.2133333333335, 405.1200000000001), (919.8933333333334, 291.83999999999986, 720.2133333333335, 405.1200000000001), (1686.186666666667, 291.83999999999986, 720.2133333333335, 405.1200000000001), (536.7466666666666, 743.04, 720.2133333333335, 405.1200000000001), (1303.04, 743.04, 720.2133333333335, 405.1200000000001)]),
        (2, 800.0, 600.0, 1.3333333333333333, 2, vec![(48.0, 170.7, 344.8, 258.6), (407.2, 170.7, 344.8, 258.6)]),
        ];
        for (n, width, height, aspect, cols, expected) in cases {
            let (rects, got_cols) = card_layout(n, width, height, aspect);
            assert_eq!(got_cols, cols, "n={n} {width}x{height}");
            assert_eq!(rects.len(), expected.len());
            for (r, e) in rects.iter().zip(&expected) {
                assert!(
                    (r.x - e.0).abs() < 1e-9
                        && (r.y - e.1).abs() < 1e-9
                        && (r.w - e.2).abs() < 1e-9
                        && (r.h - e.3).abs() < 1e-9,
                    "n={n} {width}x{height}: got {r:?}, expected {e:?}"
                );
            }
        }
    }

    #[test]
    fn quote_escapes_backslashes_and_quotes() {
        assert_eq!(quote(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(quote("plain"), "\"plain\"");
    }

    #[test]
    fn contains_handles_empty_and_missing_needles() {
        assert!(contains(b"abc", b""));
        assert!(contains(b"swayflow\0overview\0", b"overview"));
        assert!(!contains(b"abc", b"abcd"));
    }
}
