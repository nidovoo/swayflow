//! GTK 3 + wlr-layer-shell front-end of the overview.
//!
//! A full-screen overlay on the focused output draws one card per workspace
//! (scaled-down window rectangles with application icons). Keyboard and mouse
//! select a workspace or a window; the chosen Sway command is returned to the caller.
//!
//! Linking note: `libgtk-layer-shell` must come *before* `libwayland-client` in the
//! process' library order; if the overlay shows up as an ordinary window, start it with
//! `LD_PRELOAD=/usr/lib/libgtk-layer-shell.so swayflow overview`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::f64::consts::PI;
use std::rc::Rc;

use gtk::glib::translate::IntoGlib;
use gtk::prelude::*;
use gtk::{cairo, gdk, gdk_pixbuf, gio, glib};
use gtk_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use super::color::Color;
use super::theme::{ColorSet, Palette, Role, Style};
use super::{card_layout, quote, CardRect, OverviewData};
use crate::error::{Error, Result};

// X11 keysyms (stable numeric values, identical to `Gdk.KEY_*`).
const KEY_SPACE: u32 = 0x20;
const KEY_H: u32 = 0x68;
const KEY_J: u32 = 0x6a;
const KEY_K: u32 = 0x6b;
const KEY_L: u32 = 0x6c;
const KEY_Q: u32 = 0x71;
const KEY_TAB: u32 = 0xff09;
const KEY_RETURN: u32 = 0xff0d;
const KEY_ESCAPE: u32 = 0xff1b;
const KEY_LEFT: u32 = 0xff51;
const KEY_UP: u32 = 0xff52;
const KEY_RIGHT: u32 = 0xff53;
const KEY_DOWN: u32 = 0xff54;
const KEY_KP_ENTER: u32 = 0xff8d;
const KEY_ISO_LEFT_TAB: u32 = 0xfe20;

fn set_color(cr: &cairo::Context, c: Color) {
    cr.set_source_rgba(c.r, c.g, c.b, c.a);
}

/// Colours of the active GTK theme: the background, the text colour (used to
/// derive the card/window shades) and the selection colour (the accent).
/// Every GTK 3 theme defines these as `@define-color` names; the extra names
/// cover libadwaita-style themes. Returns `None` if the theme offers none of them.
fn gtk_theme_layer(window: &gtk::Window) -> Option<ColorSet> {
    let context = window.style_context();
    let lookup = |names: &[&str]| -> Option<Color> {
        names
            .iter()
            .find_map(|name| context.lookup_color(name))
            .map(|c| Color::rgb(c.red(), c.green(), c.blue()))
    };
    let background = lookup(&["theme_bg_color", "window_bg_color", "bg_color"]);
    let foreground = lookup(&["theme_fg_color", "window_fg_color", "fg_color"]);
    let accent = lookup(&[
        "theme_selected_bg_color",
        "accent_bg_color",
        "accent_color",
        "selected_bg_color",
    ]);

    let mut layer = ColorSet::default();
    if let Some(color) = background {
        layer.set(Role::Background, color);
    }
    if let Some(color) = accent {
        layer.set(Role::Accent, color);
    }
    layer.derive_surfaces(foreground);
    if layer.is_empty() {
        None
    } else {
        Some(layer)
    }
}

/// Rounded-rectangle path.
fn rrect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * PI / 2.0);
    cr.close_path();
}

/// Maps `app_id`/class to an icon surface (desktop-file lookup, then theme names).
struct IconProvider {
    /// lower-cased app id / StartupWMClass / last id segment -> icon name or path
    index: HashMap<String, String>,
    theme: Option<gtk::IconTheme>,
    cache: HashMap<(String, i32, i32), Option<cairo::Surface>>,
}

impl IconProvider {
    fn new() -> IconProvider {
        let mut index: HashMap<String, String> = HashMap::new();
        for app in gio::AppInfo::all() {
            let Some(icon) = app.icon() else {
                continue;
            };
            let icon_name = match gio::prelude::IconExt::to_string(&icon) {
                Some(name) if !name.is_empty() => name.to_string(),
                _ => continue,
            };
            let mut app_id = app.id().map(|s| s.to_string()).unwrap_or_default();
            if let Some(stripped) = app_id.strip_suffix(".desktop") {
                app_id = stripped.to_owned();
            }
            let wm_class = app
                .downcast_ref::<gio::DesktopAppInfo>()
                .and_then(|desktop| desktop.startup_wm_class())
                .map(|s| s.to_string())
                .unwrap_or_default();
            let last_segment = app_id.rsplit('.').next().unwrap_or("").to_owned();
            for ident in [app_id, wm_class, last_segment] {
                if !ident.is_empty() {
                    index
                        .entry(ident.to_lowercase())
                        .or_insert_with(|| icon_name.clone());
                }
            }
        }
        IconProvider {
            index,
            theme: gtk::IconTheme::default(),
            cache: HashMap::new(),
        }
    }

    fn load(&self, name: &str, size: i32, scale: i32) -> Option<cairo::Surface> {
        if name.starts_with('/') {
            let px = size * scale;
            let pixbuf = gdk_pixbuf::Pixbuf::from_file_at_size(name, px, px).ok()?;
            pixbuf.create_surface(scale, None::<&gdk::Window>)
        } else {
            self.theme
                .as_ref()?
                .load_surface(
                    name,
                    size,
                    scale,
                    None::<&gdk::Window>,
                    gtk::IconLookupFlags::FORCE_SIZE,
                )
                .ok()
                .flatten()
        }
    }

    fn surface(&mut self, key: &str, size: i32, scale: i32) -> Option<cairo::Surface> {
        let cache_key = (key.to_owned(), size, scale);
        if let Some(hit) = self.cache.get(&cache_key) {
            return hit.clone();
        }
        let lower = key.to_lowercase();
        let mut names: Vec<String> = Vec::new();
        if let Some(indexed) = self.index.get(&lower) {
            names.push(indexed.clone());
        }
        names.push(key.to_owned());
        names.push(lower);
        names.push("application-x-executable".to_owned());

        let surface = names.iter().find_map(|name| self.load(name, size, scale));
        self.cache.insert(cache_key, surface.clone());
        surface
    }
}

/// Everything the signal handlers share.
struct Overview {
    data: OverviewData,
    palette: Palette,
    icons: IconProvider,
    /// Output aspect ratio — cards get the same shape as the screen.
    aspect: f64,
    /// Index of the highlighted workspace.
    sel: usize,
    size: Option<(i32, i32)>,
    rects: Vec<CardRect>,
    cols: usize,
    scale_factor: i32,
    /// Sway command to run after the overlay closes.
    result: Option<String>,
}

impl Overview {
    fn new(data: OverviewData, palette: Palette) -> Overview {
        let aspect = data.rect.w as f64 / data.rect.h as f64;
        let sel = data.current.unwrap_or(0);
        Overview {
            data,
            palette,
            icons: IconProvider::new(),
            aspect,
            sel,
            size: None,
            rects: Vec::new(),
            cols: 1,
            scale_factor: 1,
            result: None,
        }
    }

    fn ensure_layout(&mut self, width: i32, height: i32, scale_factor: i32) {
        if self.size == Some((width, height)) {
            return;
        }
        self.size = Some((width, height));
        let (rects, cols) = card_layout(
            self.data.workspaces.len(),
            f64::from(width),
            f64::from(height),
            self.aspect,
        );
        self.rects = rects;
        self.cols = cols;
        self.scale_factor = scale_factor.max(1);
    }

    fn draw(
        &mut self,
        cr: &cairo::Context,
        width: i32,
        height: i32,
        scale_factor: i32,
    ) -> std::result::Result<(), cairo::Error> {
        self.ensure_layout(width, height, scale_factor);
        cr.set_operator(cairo::Operator::Source);
        set_color(cr, self.palette.dim);
        cr.paint()?;
        cr.set_operator(cairo::Operator::Over);

        let ow = self.data.rect.w as f64;
        for (i, ws) in self.data.workspaces.iter().enumerate() {
            let Some(card) = self.rects.get(i).copied() else {
                continue;
            };
            let CardRect { x: cx, y: cy, w: cw, h: ch } = card;
            let s = cw / ow;
            let radius = (cw * 0.014).max(8.0);

            rrect(cr, cx, cy, cw, ch, radius);
            set_color(cr, self.palette.card);
            cr.fill()?;

            cr.save()?;
            rrect(cr, cx, cy, cw, ch, radius);
            cr.clip();
            let inset = (cw * 0.003).max(1.0);
            let win_radius = (cw * 0.008).max(3.0);
            for win in &ws.wins {
                let wx = cx + win.x as f64 * s + inset;
                let wy = cy + win.y as f64 * s + inset;
                let ww = win.w as f64 * s - 2.0 * inset;
                let wh = win.h as f64 * s - 2.0 * inset;
                if ww < 1.0 || wh < 1.0 {
                    continue;
                }
                rrect(cr, wx, wy, ww, wh, win_radius);
                set_color(
                    cr,
                    if win.floating {
                        self.palette.win_float
                    } else {
                        self.palette.win
                    },
                );
                cr.fill()?;
                if win.focused {
                    rrect(cr, wx, wy, ww, wh, win_radius);
                    set_color(cr, self.palette.accent_soft);
                    cr.fill()?;
                }
                let side = ww.min(wh);
                if side >= 28.0 {
                    let icon_size = (side * 0.5).min(128.0).max(16.0) as i32;
                    if let Some(surface) =
                        self.icons.surface(&win.key, icon_size, self.scale_factor)
                    {
                        let isz = f64::from(icon_size);
                        cr.set_source_surface(
                            &surface,
                            wx + (ww - isz) / 2.0,
                            wy + (wh - isz) / 2.0,
                        )?;
                        cr.paint()?;
                    }
                }
            }
            cr.restore()?;

            rrect(cr, cx, cy, cw, ch, radius);
            cr.set_line_width(2.0);
            set_color(
                cr,
                if i == self.sel {
                    self.palette.accent
                } else {
                    self.palette.border
                },
            );
            cr.stroke()?;
        }
        Ok(())
    }

    /// Which card (and which window inside it) is under the pointer.
    fn hit(&self, px: f64, py: f64) -> (Option<usize>, Option<i64>) {
        let ow = self.data.rect.w as f64;
        for (i, ws) in self.data.workspaces.iter().enumerate() {
            let Some(c) = self.rects.get(i) else {
                continue;
            };
            if !(c.x <= px && px <= c.x + c.w && c.y <= py && py <= c.y + c.h) {
                continue;
            }
            let s = c.w / ow;
            for win in ws.wins.iter().rev() {
                let (wx, wy) = (c.x + win.x as f64 * s, c.y + win.y as f64 * s);
                if wx <= px
                    && px <= wx + win.w as f64 * s
                    && wy <= py
                    && py <= wy + win.h as f64 * s
                {
                    return (Some(i), Some(win.id));
                }
            }
            return (Some(i), None);
        }
        (None, None)
    }

    fn finish(&mut self, result: Option<String>) {
        self.result = result;
        gtk::main_quit();
    }

    fn pick_workspace(&mut self, index: usize) {
        let command = match self.data.workspaces.get(index) {
            Some(ws) if !ws.visible => Some(format!("workspace {}", quote(&ws.name))),
            _ => None,
        };
        self.finish(command);
    }

    /// Moves the selection; returns whether a redraw is needed.
    fn step(&mut self, delta: isize) -> bool {
        let new = self.sel as isize + delta;
        if new >= 0 && (new as usize) < self.data.workspaces.len() {
            self.sel = new as usize;
            true
        } else {
            false
        }
    }

    /// Handles a key press; returns whether a redraw is needed.
    fn handle_key(&mut self, key: u32) -> bool {
        let cols = self.cols as isize;
        match key {
            KEY_ESCAPE | KEY_Q => self.finish(None),
            KEY_LEFT | KEY_H | KEY_ISO_LEFT_TAB => return self.step(-1),
            KEY_RIGHT | KEY_L | KEY_TAB => return self.step(1),
            KEY_UP | KEY_K => return self.step(-cols),
            KEY_DOWN | KEY_J => return self.step(cols),
            KEY_RETURN | KEY_KP_ENTER | KEY_SPACE => self.pick_workspace(self.sel),
            0x30..=0x39 => {
                let num = i64::from(key - 0x30);
                if let Some(index) = self.data.workspaces.iter().position(|w| w.num == num) {
                    self.pick_workspace(index);
                }
            }
            _ => {}
        }
        false
    }
}

/// Shows the overlay and blocks until it is closed.
/// Returns the Sway command to execute (`None` = just close).
pub fn run_overview(data: OverviewData, style: &Style) -> Result<Option<String>> {
    gtk::init().map_err(|e| {
        Error::runtime(format!(
            "missing dependency (gtk3, gtk-layer-shell) or no display: {e}"
        ))
    })?;
    if !gtk_layer_shell::is_supported() {
        return Err(Error::runtime("compositor does not support wlr-layer-shell"));
    }
    let output = data.rect;

    let css = gtk::CssProvider::new();
    css.load_from_data(b"window, drawingarea { background: transparent; }")
        .map_err(|e| Error::runtime(format!("cannot load CSS: {e}")))?;
    let screen = gdk::Screen::default();
    if let Some(screen) = &screen {
        gtk::StyleContext::add_provider_for_screen(
            screen,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_title("swayflow-overview");
    window.set_decorated(false);
    window.set_app_paintable(true);
    if let Some(visual) = screen.as_ref().and_then(|s| s.rgba_visual()) {
        window.set_visual(Some(&visual));
    }

    window.init_layer_shell();
    window.set_namespace("swayflow-overview");
    window.set_layer(Layer::Overlay);
    for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
        window.set_anchor(edge, true);
    }
    window.set_exclusive_zone(-1);
    window.set_keyboard_mode(KeyboardMode::Exclusive);
    if let Some(display) = gdk::Display::default() {
        for i in 0..display.n_monitors() {
            let Some(monitor) = display.monitor(i) else {
                continue;
            };
            let g = monitor.geometry();
            let geometry = (
                i64::from(g.x()),
                i64::from(g.y()),
                i64::from(g.width()),
                i64::from(g.height()),
            );
            if geometry == (output.x, output.y, output.w, output.h) {
                window.set_monitor(&monitor);
                break;
            }
        }
    }

    let gtk_layer = if style.use_gtk {
        let layer = gtk_theme_layer(&window);
        if layer.is_none() {
            eprintln!(
                "swayflow: the GTK theme does not define the expected colours; using the built-in palette"
            );
        }
        layer
    } else {
        None
    };
    let palette = style.palette(gtk_layer.as_ref());

    let overview = Rc::new(RefCell::new(Overview::new(data, palette)));

    let area = gtk::DrawingArea::new();
    area.add_events(gdk::EventMask::BUTTON_PRESS_MASK | gdk::EventMask::POINTER_MOTION_MASK);

    area.connect_draw({
        let overview = Rc::clone(&overview);
        move |widget, cr| {
            let (w, h, sf) = (
                widget.allocated_width(),
                widget.allocated_height(),
                widget.scale_factor(),
            );
            if let Err(err) = overview.borrow_mut().draw(cr, w, h, sf) {
                eprintln!("swayflow: drawing failed: {err}");
            }
            glib::Propagation::Proceed
        }
    });

    area.connect_motion_notify_event({
        let overview = Rc::clone(&overview);
        move |widget, event| {
            let (px, py) = event.position();
            let mut ov = overview.borrow_mut();
            if let (Some(index), _) = ov.hit(px, py) {
                if index != ov.sel {
                    ov.sel = index;
                    widget.queue_draw();
                }
            }
            glib::Propagation::Stop
        }
    });

    area.connect_button_press_event({
        let overview = Rc::clone(&overview);
        move |_, event| {
            let (px, py) = event.position();
            let mut ov = overview.borrow_mut();
            if event.button() != 1 {
                ov.finish(None);
                return glib::Propagation::Stop;
            }
            match ov.hit(px, py) {
                (None, _) => ov.finish(None),
                (Some(_), Some(win_id)) => ov.finish(Some(format!("[con_id={win_id}] focus"))),
                (Some(index), None) => ov.pick_workspace(index),
            }
            glib::Propagation::Stop
        }
    });

    window.add(&area);
    window.add_events(gdk::EventMask::KEY_PRESS_MASK);
    window.connect_key_press_event({
        let overview = Rc::clone(&overview);
        let area = area.downgrade();
        move |_, event| {
            let key: u32 = event.keyval().into_glib();
            let redraw = overview.borrow_mut().handle_key(key);
            if redraw {
                if let Some(area) = area.upgrade() {
                    area.queue_draw();
                }
            }
            glib::Propagation::Stop
        }
    });

    for signum in [libc::SIGTERM, libc::SIGINT] {
        glib::unix_signal_add(signum, || {
            gtk::main_quit();
            glib::ControlFlow::Break
        });
    }

    window.show_all();
    gtk::main();
    let result = overview.borrow_mut().result.take();
    window.close();
    while gtk::events_pending() {
        gtk::main_iteration();
    }
    Ok(result)
}
