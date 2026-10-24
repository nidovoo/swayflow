//! Pure geometry: rectangles, auto-placement layouts and geometry restoration.
//!
//! Everything here is deterministic integer/float arithmetic without I/O, so it
//! is covered by unit tests whose expected values come from the reference
//! Python implementation.
//!
//! Notes on exact equivalence with the Python original:
//! * `int(x)` truncates toward zero  -> `x as i64`;
//! * `round(x)` is round-half-to-even -> `f64::round_ties_even`;
//! * `a // b` floors                  -> `div_euclid` (divisors are positive);
//! * `min(max(v, lo), hi)` lets `hi` win when `lo > hi` -> `v.max(lo).min(hi)`
//!   (deliberately *not* `clamp`, which panics in that case).

const MIN_W: i64 = 320;
const MIN_H: i64 = 220;
const SINGLE_RATIO: f64 = 0.72;
const CASCADE_STEP: i64 = 40;
const SCAN_STEP: i64 = 24;
/// How many pixels of a restored window must remain inside the workspace.
const MIN_VISIBLE: i64 = 80;
/// Rects closer than this (per component) count as "untouched since auto-placement".
const TOLERANCE: i64 = 6;
/// Largest workspace-edge distance that is still interpreted as a compositor gap.
const MAX_DETECTED_GAP: i64 = 64;

/// Outer margin and inter-window gap used by the layouts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spacing {
    pub margin: i64,
    pub gap: i64,
}

impl Spacing {
    /// Python defaults: `MARGIN = 24`, `GAP = 16`.
    pub const DEFAULT: Spacing = Spacing { margin: 24, gap: 16 };

    /// Python `set_gap(value)`: margin and gap both become `max(4, value)`.
    pub fn uniform(value: i64) -> Spacing {
        let v = value.max(4);
        Spacing { margin: v, gap: v }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
}

impl Rect {
    pub const fn new(x: i64, y: i64, w: i64, h: i64) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn from_array([x, y, w, h]: [i64; 4]) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn to_array(self) -> [i64; 4] {
        [self.x, self.y, self.w, self.h]
    }
}

/// Geometry saved when a workspace is switched back to tiling:
/// the window rect relative to the workspace plus the workspace size at that time.
#[derive(Clone, Copy, Debug)]
pub struct SavedGeom {
    pub rect: [f64; 4],
    pub area: [f64; 2],
}

pub fn intersects(a: Rect, b: Rect, pad: i64) -> bool {
    a.x < b.x + b.w + pad && b.x < a.x + a.w + pad && a.y < b.y + b.h + pad && b.y < a.y + a.h + pad
}

pub fn close_rects(a: Rect, b: Rect) -> bool {
    (a.x - b.x).abs() <= TOLERANCE
        && (a.y - b.y).abs() <= TOLERANCE
        && (a.w - b.w).abs() <= TOLERANCE
        && (a.h - b.h).abs() <= TOLERANCE
}

/// The `i`-th slot of the diagonal cascade.
fn cascade_slot(area: Rect, i: usize, sp: Spacing) -> Rect {
    let uw = (area.w - 2 * sp.margin).max(1);
    let uh = (area.h - 2 * sp.margin).max(1);
    let w = uw.min(MIN_W.max((area.w as f64 * 0.6) as i64));
    let h = uh.min(MIN_H.max((area.h as f64 * 0.65) as i64));
    let span_x = (uw - w).max(1);
    let span_y = (uh - h).max(1);
    let k = i as i64 * CASCADE_STEP;
    Rect::new(
        area.x + sp.margin + k.rem_euclid(span_x),
        area.y + sp.margin + k.rem_euclid(span_y),
        w,
        h,
    )
}

pub fn cascade_layout(n: usize, area: Rect, start: usize, sp: Spacing) -> Vec<Rect> {
    (0..n).map(|i| cascade_slot(area, start + i, sp)).collect()
}

/// Picks the column count whose cells are the largest and closest to 4:3,
/// then centres every row. Falls back to a cascade when cells would be too small.
pub fn grid_layout(n: usize, area: Rect, sp: Spacing) -> Vec<Rect> {
    let uw = area.w - 2 * sp.margin;
    let uh = area.h - 2 * sp.margin;
    if uw <= 0 || uh <= 0 {
        return cascade_layout(n, area, 0, sp);
    }
    if n == 1 {
        let w = (uw as f64 * SINGLE_RATIO) as i64;
        let h = (uh as f64 * SINGLE_RATIO) as i64;
        return vec![Rect::new(
            area.x + (area.w - w).div_euclid(2),
            area.y + (area.h - h).div_euclid(2),
            w,
            h,
        )];
    }

    // (score, cols, rows, cell width, cell height)
    let mut best: Option<(f64, usize, usize, f64, f64)> = None;
    for cols in 1..=n {
        let rows = n.div_ceil(cols);
        let cw = (uw - (cols as i64 - 1) * sp.gap) as f64 / cols as f64;
        let ch = (uh - (rows as i64 - 1) * sp.gap) as f64 / rows as f64;
        if cw < 1.0 || ch < 1.0 {
            continue;
        }
        let score = cw * ch * (-((cw / ch) / (4.0_f64 / 3.0)).ln().abs()).exp();
        if best.map_or(true, |b| score > b.0) {
            best = Some((score, cols, rows, cw, ch));
        }
    }
    let Some((_, cols, rows, cw, ch)) = best else {
        return cascade_layout(n, area, 0, sp);
    };
    if cw < MIN_W as f64 || ch < MIN_H as f64 {
        return cascade_layout(n, area, 0, sp);
    }
    let (cw, ch) = (cw as i64, ch as i64);

    (0..n)
        .map(|i| {
            let (r, c) = (i / cols, i % cols);
            let cnt = (if r < rows - 1 { cols } else { n - cols * (rows - 1) }) as i64;
            let row_w = cnt * cw + (cnt - 1) * sp.gap;
            let x0 = area.x + sp.margin + (uw - row_w).div_euclid(2);
            Rect::new(
                x0 + c as i64 * (cw + sp.gap),
                area.y + sp.margin + r as i64 * (ch + sp.gap),
                cw,
                ch,
            )
        })
        .collect()
}

/// First free `w`×`h` spot found by scanning the area row by row.
fn find_free(occupied: &[Rect], area: Rect, w: i64, h: i64, sp: Spacing) -> Option<Rect> {
    let x_max = area.x + area.w - sp.margin - w;
    let y_max = area.y + area.h - sp.margin - h;
    let mut y = area.y + sp.margin;
    while y <= y_max {
        let mut x = area.x + sp.margin;
        while x <= x_max {
            let cand = Rect::new(x, y, w, h);
            if !occupied.iter().any(|&o| intersects(cand, o, sp.gap)) {
                return Some(cand);
            }
            x += SCAN_STEP;
        }
        y += SCAN_STEP;
    }
    None
}

/// Places `n` new windows into the free space left between `occupied` rects.
/// `total` is the number of windows the workspace will hold (used to size cells).
pub fn scan_layout(n: usize, area: Rect, occupied: &[Rect], total: usize, sp: Spacing) -> Vec<Rect> {
    let uw = (area.w - 2 * sp.margin).max(1);
    let uh = (area.h - 2 * sp.margin).max(1);
    let total = total.max(1) as i64;
    let cols = ((total * uw) as f64 / uh as f64)
        .sqrt()
        .round_ties_even() as i64;
    let cols = cols.min(total).max(1);
    let rows = (total + cols - 1) / cols;
    let base_w = uw.min(MIN_W.max(((uw - (cols - 1) * sp.gap) as f64 / cols as f64) as i64));
    let base_h = uh.min(MIN_H.max(((uh - (rows - 1) * sp.gap) as f64 / rows as f64) as i64));

    let mut occ: Vec<Rect> = occupied.to_vec();
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let mut spot = None;
        for k in [1.0_f64, 0.85, 0.7, 0.55] {
            let w = uw.min(MIN_W.max((base_w as f64 * k) as i64));
            let h = uh.min(MIN_H.max((base_h as f64 * k) as i64));
            spot = find_free(&occ, area, w, h, sp);
            if spot.is_some() {
                break;
            }
        }
        let spot = spot.unwrap_or_else(|| cascade_slot(area, occ.len(), sp));
        occ.push(spot);
        out.push(spot);
    }
    out
}

/// Re-creates a window rect saved relative to the workspace, scaling it when the
/// workspace size changed and keeping at least `MIN_VISIBLE` px on screen.
pub fn restore_rect(saved: &SavedGeom, area: Rect) -> Rect {
    let [mut rx, mut ry, mut w, mut h] = saved.rect;
    let [sw, sh] = saved.area;
    let (aw, ah) = (area.w as f64, area.h as f64);
    if (sw != aw || sh != ah) && sw > 0.0 && sh > 0.0 {
        let (kx, ky) = (aw / sw, ah / sh);
        rx *= kx;
        ry *= ky;
        w *= kx;
        h *= ky;
    }
    let w = (w.round_ties_even() as i64).max(1);
    let h = (h.round_ties_even() as i64).max(1);
    let x = area.x + rx.round_ties_even() as i64;
    let y = area.y + ry.round_ties_even() as i64;
    let x = x.max(area.x - w + MIN_VISIBLE).min(area.x + area.w - MIN_VISIBLE);
    let y = y.max(area.y).min(area.y + area.h - MIN_VISIBLE);
    Rect::new(x, y, w, h)
}

/// Smallest positive distance between a tiled window and the workspace edge —
/// that is the compositor's `gaps outer`/`inner` setting. `None` if it exceeds 64 px.
pub fn detect_gap(area: Rect, tiled: &[Rect]) -> Option<i64> {
    let mut best: Option<i64> = None;
    for r in tiled {
        let distances = [
            r.x - area.x,
            r.y - area.y,
            area.x + area.w - r.x - r.w,
            area.y + area.h - r.y - r.h,
        ];
        for d in distances {
            if d > 0 && best.map_or(true, |b| d < b) {
                best = Some(d);
            }
        }
    }
    best.filter(|&b| b <= MAX_DETECTED_GAP)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected values below were produced by the reference Python implementation (swayflow_ver4.py).

    #[test]
    fn grid_layout_matches_reference() {
        let cases: Vec<(usize, Rect, Spacing, Vec<Rect>)> = vec![
        (0, Rect::new(0, 30, 1920, 1050), Spacing::DEFAULT, vec![]),
        (1, Rect::new(0, 30, 1920, 1050), Spacing::DEFAULT, vec![Rect::new(286, 194, 1347, 721)]),
        (2, Rect::new(0, 30, 1920, 1050), Spacing::DEFAULT, vec![Rect::new(24, 54, 928, 1002), Rect::new(968, 54, 928, 1002)]),
        (3, Rect::new(0, 30, 1920, 1050), Spacing::DEFAULT, vec![Rect::new(24, 54, 928, 493), Rect::new(968, 54, 928, 493), Rect::new(496, 563, 928, 493)]),
        (4, Rect::new(0, 30, 1920, 1050), Spacing::DEFAULT, vec![Rect::new(24, 54, 928, 493), Rect::new(968, 54, 928, 493), Rect::new(24, 563, 928, 493), Rect::new(968, 563, 928, 493)]),
        (5, Rect::new(0, 30, 1920, 1050), Spacing::DEFAULT, vec![Rect::new(24, 54, 613, 493), Rect::new(653, 54, 613, 493), Rect::new(1282, 54, 613, 493), Rect::new(339, 563, 613, 493), Rect::new(968, 563, 613, 493)]),
        (6, Rect::new(0, 30, 1920, 1050), Spacing::DEFAULT, vec![Rect::new(24, 54, 613, 493), Rect::new(653, 54, 613, 493), Rect::new(1282, 54, 613, 493), Rect::new(24, 563, 613, 493), Rect::new(653, 563, 613, 493), Rect::new(1282, 563, 613, 493)]),
        (7, Rect::new(0, 30, 1920, 1050), Spacing::DEFAULT, vec![Rect::new(24, 54, 456, 493), Rect::new(496, 54, 456, 493), Rect::new(968, 54, 456, 493), Rect::new(1440, 54, 456, 493), Rect::new(260, 563, 456, 493), Rect::new(732, 563, 456, 493), Rect::new(1204, 563, 456, 493)]),
        (1, Rect::new(0, 30, 1920, 1050), Spacing { margin: 12, gap: 12 }, vec![Rect::new(277, 186, 1365, 738)]),
        (2, Rect::new(0, 30, 1920, 1050), Spacing { margin: 12, gap: 12 }, vec![Rect::new(12, 42, 942, 1026), Rect::new(966, 42, 942, 1026)]),
        (3, Rect::new(0, 30, 1920, 1050), Spacing { margin: 12, gap: 12 }, vec![Rect::new(12, 42, 942, 507), Rect::new(966, 42, 942, 507), Rect::new(489, 561, 942, 507)]),
        (4, Rect::new(0, 30, 1920, 1050), Spacing { margin: 12, gap: 12 }, vec![Rect::new(12, 42, 942, 507), Rect::new(966, 42, 942, 507), Rect::new(12, 561, 942, 507), Rect::new(966, 561, 942, 507)]),
        (5, Rect::new(0, 30, 1920, 1050), Spacing { margin: 12, gap: 12 }, vec![Rect::new(12, 42, 624, 507), Rect::new(648, 42, 624, 507), Rect::new(1284, 42, 624, 507), Rect::new(330, 561, 624, 507), Rect::new(966, 561, 624, 507)]),
        (1, Rect::new(1920, 0, 2560, 1440), Spacing { margin: 12, gap: 12 }, vec![Rect::new(2287, 210, 1825, 1019)]),
        (2, Rect::new(1920, 0, 2560, 1440), Spacing { margin: 12, gap: 12 }, vec![Rect::new(1932, 12, 1262, 1416), Rect::new(3206, 12, 1262, 1416)]),
        (3, Rect::new(1920, 0, 2560, 1440), Spacing { margin: 12, gap: 12 }, vec![Rect::new(1932, 12, 1262, 702), Rect::new(3206, 12, 1262, 702), Rect::new(2569, 726, 1262, 702)]),
        (4, Rect::new(1920, 0, 2560, 1440), Spacing { margin: 12, gap: 12 }, vec![Rect::new(1932, 12, 1262, 702), Rect::new(3206, 12, 1262, 702), Rect::new(1932, 726, 1262, 702), Rect::new(3206, 726, 1262, 702)]),
        (5, Rect::new(1920, 0, 2560, 1440), Spacing { margin: 12, gap: 12 }, vec![Rect::new(1932, 12, 837, 702), Rect::new(2781, 12, 837, 702), Rect::new(3630, 12, 837, 702), Rect::new(2357, 726, 837, 702), Rect::new(3206, 726, 837, 702)]),
        (6, Rect::new(1920, 0, 2560, 1440), Spacing { margin: 12, gap: 12 }, vec![Rect::new(1932, 12, 837, 702), Rect::new(2781, 12, 837, 702), Rect::new(3630, 12, 837, 702), Rect::new(1932, 726, 837, 702), Rect::new(2781, 726, 837, 702), Rect::new(3630, 726, 837, 702)]),
        (4, Rect::new(0, 0, 1366, 768), Spacing::DEFAULT, vec![Rect::new(24, 24, 651, 352), Rect::new(691, 24, 651, 352), Rect::new(24, 392, 651, 352), Rect::new(691, 392, 651, 352)]),
        (6, Rect::new(0, 0, 1366, 768), Spacing::DEFAULT, vec![Rect::new(25, 24, 428, 352), Rect::new(469, 24, 428, 352), Rect::new(913, 24, 428, 352), Rect::new(25, 392, 428, 352), Rect::new(469, 392, 428, 352), Rect::new(913, 392, 428, 352)]),
        (9, Rect::new(0, 0, 1366, 768), Spacing::DEFAULT, vec![Rect::new(24, 24, 819, 499), Rect::new(64, 64, 819, 499), Rect::new(104, 104, 819, 499), Rect::new(144, 144, 819, 499), Rect::new(184, 184, 819, 499), Rect::new(224, 224, 819, 499), Rect::new(264, 43, 819, 499), Rect::new(304, 83, 819, 499), Rect::new(344, 123, 819, 499)]),
        (3, Rect::new(0, 0, 500, 400), Spacing::DEFAULT, vec![Rect::new(24, 24, 320, 260), Rect::new(64, 64, 320, 260), Rect::new(104, 104, 320, 260)]),
        (2, Rect::new(0, 0, 40, 40), Spacing::DEFAULT, vec![Rect::new(24, 24, 1, 1), Rect::new(24, 24, 1, 1)]),
        ];
        for (n, area, sp, expected) in cases {
            assert_eq!(grid_layout(n, area, sp), expected, "n={n} area={area:?} sp={sp:?}");
        }
    }

    #[test]
    fn scan_layout_matches_reference() {
        let cases: Vec<(usize, Rect, Vec<Rect>, usize, Spacing, Vec<Rect>)> = vec![
        (3, Rect::new(0, 30, 1920, 1050), vec![Rect::new(100, 100, 600, 400), Rect::new(800, 200, 500, 300)], 5, Spacing::DEFAULT, vec![Rect::new(24, 534, 613, 493), Rect::new(672, 534, 613, 493), Rect::new(1320, 54, 521, 419)]),
        (3, Rect::new(0, 30, 1920, 1050), vec![Rect::new(100, 100, 600, 400), Rect::new(800, 200, 500, 300)], 5, Spacing { margin: 10, gap: 10 }, vec![Rect::new(10, 520, 626, 510), Rect::new(658, 520, 626, 510), Rect::new(1330, 40, 532, 433)]),
        (2, Rect::new(0, 0, 1200, 900), vec![Rect::new(0, 0, 1200, 900)], 3, Spacing::DEFAULT, vec![Rect::new(64, 64, 720, 585), Rect::new(104, 104, 720, 585)]),
        (4, Rect::new(0, 0, 1920, 1080), vec![], 4, Spacing::DEFAULT, vec![Rect::new(24, 24, 613, 508), Rect::new(672, 24, 613, 508), Rect::new(1320, 24, 521, 431), Rect::new(1320, 480, 521, 431)]),
        (5, Rect::new(1920, 0, 2560, 1440), vec![Rect::new(1950, 30, 1200, 700)], 6, Spacing { margin: 16, gap: 16 }, vec![Rect::new(3184, 16, 832, 696), Rect::new(3184, 736, 707, 591), Rect::new(1936, 760, 707, 591), Rect::new(3928, 736, 457, 382), Rect::new(2680, 760, 457, 382)]),
        (1, Rect::new(0, 0, 1366, 768), vec![Rect::new(30, 30, 1000, 600)], 1, Spacing::DEFAULT, vec![Rect::new(64, 64, 819, 499)]),
        ];
        for (n, area, occupied, total, sp, expected) in cases {
            assert_eq!(
                scan_layout(n, area, &occupied, total, sp),
                expected,
                "n={n} area={area:?} occupied={occupied:?} sp={sp:?}"
            );
        }
    }

    #[test]
    fn cascade_layout_matches_reference() {
        let cases: Vec<(usize, Rect, usize, Spacing, Vec<Rect>)> = vec![
        (5, Rect::new(0, 30, 1920, 1050), 0, Spacing::DEFAULT, vec![Rect::new(24, 54, 1152, 682), Rect::new(64, 94, 1152, 682), Rect::new(104, 134, 1152, 682), Rect::new(144, 174, 1152, 682), Rect::new(184, 214, 1152, 682)]),
        (5, Rect::new(0, 30, 1920, 1050), 2, Spacing::DEFAULT, vec![Rect::new(104, 134, 1152, 682), Rect::new(144, 174, 1152, 682), Rect::new(184, 214, 1152, 682), Rect::new(224, 254, 1152, 682), Rect::new(264, 294, 1152, 682)]),
        (3, Rect::new(0, 0, 500, 400), 7, Spacing { margin: 8, gap: 8 }, vec![Rect::new(124, 40, 320, 260), Rect::new(164, 80, 320, 260), Rect::new(40, 120, 320, 260)]),
        (2, Rect::new(0, 0, 100, 100), 0, Spacing::DEFAULT, vec![Rect::new(24, 24, 52, 52), Rect::new(24, 24, 52, 52)]),
        (0, Rect::new(0, 30, 1920, 1050), 0, Spacing::DEFAULT, vec![]),
        ];
        for (n, area, start, sp, expected) in cases {
            assert_eq!(cascade_layout(n, area, start, sp), expected, "n={n} area={area:?} start={start}");
        }
    }

    #[test]
    fn restore_rect_matches_reference() {
        let cases: Vec<([f64; 4], [f64; 2], Rect, Rect)> = vec![
        ([5.0, 5.0, 5.0, 5.0], [2000.0, 1000.0], Rect::new(0, 0, 1000, 500), Rect::new(78, 2, 2, 2)),
        ([-300.0, 10.0, 500.0, 400.0], [1600.0, 1200.0], Rect::new(100, 50, 800, 600), Rect::new(-50, 55, 250, 200)),
        ([1590.0, 1190.0, 500.0, 400.0], [1600.0, 1200.0], Rect::new(0, 0, 1600, 1200), Rect::new(1520, 1120, 500, 400)),
        ([-1000.0, -50.0, 300.0, 300.0], [1920.0, 1080.0], Rect::new(0, 30, 1920, 1050), Rect::new(-220, 30, 300, 292)),
        ([10.5, 20.5, 300.5, 200.5], [1920.0, 1080.0], Rect::new(0, 30, 1920, 1050), Rect::new(10, 50, 300, 195)),
        ([100.0, 100.0, 640.0, 480.0], [1920.0, 1080.0], Rect::new(1920, 0, 2560, 1440), Rect::new(2053, 133, 853, 640)),
        ([100.0, 100.0, 640.0, 480.0], [0.0, 0.0], Rect::new(0, 0, 800, 600), Rect::new(100, 100, 640, 480)),
        ([0.0, 0.0, 0.0, 0.0], [1920.0, 1080.0], Rect::new(0, 0, 1920, 1080), Rect::new(79, 0, 1, 1)),
        ];
        for (r, a, area, expected) in cases {
            let saved = SavedGeom { rect: r, area: a };
            assert_eq!(restore_rect(&saved, area), expected, "saved={saved:?} area={area:?}");
        }
    }

    #[test]
    fn detect_gap_matches_reference() {
        let cases: Vec<(Rect, Vec<Rect>, Option<i64>)> = vec![
        (Rect::new(0, 30, 1920, 1050), vec![Rect::new(10, 40, 945, 1030), Rect::new(965, 40, 945, 1030)], Some(10)),
        (Rect::new(0, 30, 1920, 1050), vec![], None),
        (Rect::new(0, 30, 1920, 1050), vec![Rect::new(100, 130, 800, 800)], None),
        (Rect::new(0, 30, 1920, 1050), vec![Rect::new(0, 30, 1920, 1050)], None),
        (Rect::new(0, 0, 1000, 1000), vec![Rect::new(64, 70, 800, 800), Rect::new(5, 5, 10, 10)], Some(5)),
        ];
        for (area, tiled, expected) in cases {
            assert_eq!(detect_gap(area, &tiled), expected, "area={area:?} tiled={tiled:?}");
        }
    }

    #[test]
    fn spacing_uniform_has_a_floor_of_four() {
        assert_eq!(Spacing::uniform(2), Spacing { margin: 4, gap: 4 });
        assert_eq!(Spacing::uniform(10), Spacing { margin: 10, gap: 10 });
        assert_eq!(Spacing::DEFAULT, Spacing { margin: 24, gap: 16 });
    }

    #[test]
    fn intersects_and_close_rects() {
        let a = Rect::new(0, 0, 100, 100);
        let touching = Rect::new(100, 0, 50, 50);
        assert!(!intersects(a, touching, 0));
        assert!(intersects(a, touching, 1));
        assert!(intersects(a, Rect::new(99, 99, 10, 10), 0));
        assert!(close_rects(a, Rect::new(6, -6, 106, 94)));
        assert!(!close_rects(a, Rect::new(7, 0, 100, 100)));
    }

    #[test]
    fn rect_array_roundtrip() {
        let r = Rect::new(1, -2, 3, 4);
        assert_eq!(Rect::from_array(r.to_array()), r);
    }
}
