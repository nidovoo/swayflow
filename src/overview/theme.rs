//! Colours of the overview: roles, palette resolution, command-line options and
//! the config file.
//!
//! Colours can come from four sources; a later source overrides an earlier one:
//!
//! ```text
//! built-in palette  <  GTK theme (--theme gtk)  <  config file  <  command line
//! ```
//!
//! Every source is a [`ColorSet`]: a partial palette in which unset roles are left
//! to the sources below. A layer that sets `background` but not the surface
//! colours (`card`, `border`, `window`, `floating`) gets them derived from the
//! background, so one colour is enough to re-colour the whole overlay.
//!
//! Nothing here depends on GTK; the lookup of the GTK theme colours lives in `ui`.
#![cfg_attr(not(feature = "overview"), allow(dead_code))]

use std::fs;
use std::io;
use std::path::PathBuf;
use std::slice;

use super::color::Color;
use crate::paths::overview_config;

/// Opacity of the dimming layer when the background is given without alpha.
const DIM_ALPHA: f64 = 0.80;
/// Opacity of the built-in and derived workspace card.
const CARD_ALPHA: f64 = 0.96;
/// Opacity of the accent tint over the focused window.
const ACCENT_SOFT_ALPHA: f64 = 0.22;

pub const OVERVIEW_USAGE: &str = "usage: swayflow overview [--theme gtk|default] [--config FILE | --no-config]
                          [--background COLOR] [--card COLOR] [--border COLOR]
                          [--accent COLOR] [--window COLOR] [--floating COLOR]
try `swayflow overview --help` for details";

pub const OVERVIEW_HELP: &str = r##"usage: swayflow overview [options]

Opens the workspace overview; run it again to close it.

Colour options (VALUE is a CSS name such as steelblue, #rgb, #rrggbb, #rrggbbaa,
rgb(r,g,b) or rgba(r,g,b,a); quote values that contain # or parentheses):
  --background VALUE   dim layer over the desktop   (default #0b0e13, 80% opaque)
  --card VALUE         workspace card               (default #0f1217)
  --border VALUE       card border                  (default #2e3842)
  --accent VALUE       selection border, focus tint (default #88c0d0)
  --window VALUE       tiled window                 (default #1b2027)
  --floating VALUE     floating window              (default #262c32)

Other options:
  --theme gtk|default  take the base colours from the active GTK theme
  --config FILE        read colours from FILE instead of
                       $XDG_CONFIG_HOME/swayflow/overview.conf
  --no-config          do not read the config file
  -h, --help           show this help

Colours are taken, from lowest to highest priority, from: the built-in palette,
the GTK theme, the config file, the command line. A source that sets --background
but not card, border, window or floating gets those derived from it."##;

/// The colours that can be configured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Dimming layer over the whole screen.
    Background,
    /// Workspace card.
    Card,
    /// Card border.
    Border,
    /// Selected card border and the tint over the focused window.
    Accent,
    /// Tiled window.
    Window,
    /// Floating window.
    Floating,
}

impl Role {
    fn index(self) -> usize {
        match self {
            Role::Background => 0,
            Role::Card => 1,
            Role::Border => 2,
            Role::Accent => 3,
            Role::Window => 4,
            Role::Floating => 5,
        }
    }

    /// Option / config-file name of the role.
    pub fn from_key(key: &str) -> Option<Role> {
        match key {
            "background" => Some(Role::Background),
            "card" => Some(Role::Card),
            "border" => Some(Role::Border),
            "accent" => Some(Role::Accent),
            "window" => Some(Role::Window),
            "floating" => Some(Role::Floating),
            _ => None,
        }
    }
}

/// The built-in colour of a role.
fn default_color(role: Role) -> Color {
    match role {
        Role::Background => Color::from_hex(0x0b0e13),
        Role::Card => Color::from_hex(0x0f1217).with_alpha(CARD_ALPHA),
        Role::Border => Color::from_hex(0x2e3842),
        Role::Accent => Color::from_hex(0x88c0d0),
        Role::Window => Color::from_hex(0x1b2027),
        Role::Floating => Color::from_hex(0x262c32),
    }
}

/// A partial palette: the colours one source (GTK theme, config file or command
/// line) has an opinion about.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ColorSet {
    colors: [Option<Color>; 6],
}

impl ColorSet {
    pub fn get(&self, role: Role) -> Option<Color> {
        self.colors[role.index()]
    }

    pub fn set(&mut self, role: Role, color: Color) {
        self.colors[role.index()] = Some(color);
    }

    pub fn is_empty(&self) -> bool {
        self.colors.iter().all(Option::is_none)
    }

    /// The colour of `role`, falling back to the built-in one.
    fn resolve(&self, role: Role) -> Color {
        self.get(role).unwrap_or_else(|| default_color(role))
    }

    /// Lets every colour set in `upper` replace the one in `self`.
    pub fn overlay(&mut self, upper: &ColorSet) {
        for (slot, over) in self.colors.iter_mut().zip(upper.colors.iter()) {
            if over.is_some() {
                *slot = *over;
            }
        }
    }

    /// If the layer sets a background, fills in the surface colours it does not
    /// set itself by blending the background towards the foreground (the theme's
    /// text colour, or white/black picked by the background's brightness).
    pub fn derive_surfaces(&mut self, foreground: Option<Color>) {
        let Some(background) = self.get(Role::Background) else {
            return;
        };
        let light = background.luminance() >= 0.5;
        let foreground = foreground.unwrap_or(if light {
            Color::rgb(0.0, 0.0, 0.0)
        } else {
            Color::rgb(1.0, 1.0, 1.0)
        });
        // Light backgrounds need a bigger step to stay as readable as dark ones.
        let boost = if light { 1.6 } else { 1.0 };
        let base = Color::rgb(background.r, background.g, background.b);
        let steps = [
            (Role::Card, 0.02),
            (Role::Window, 0.07),
            (Role::Floating, 0.12),
            (Role::Border, 0.20),
        ];
        for (role, amount) in steps {
            if self.get(role).is_none() {
                let mut color = base.mix(foreground, amount * boost);
                if role == Role::Card {
                    color = color.with_alpha(CARD_ALPHA);
                }
                self.set(role, color);
            }
        }
    }
}

/// The colours the overlay is drawn with.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dim: Color,
    pub card: Color,
    pub border: Color,
    pub accent: Color,
    pub accent_soft: Color,
    pub win: Color,
    pub win_float: Color,
}

impl Palette {
    /// Resolves a merged layer; roles nobody set get the built-in colour.
    pub fn from_layer(layer: &ColorSet) -> Palette {
        let background = layer.resolve(Role::Background);
        // A background without explicit alpha is a dimming layer, not a wall.
        let dim = if background.has_alpha {
            background
        } else {
            background.with_alpha(DIM_ALPHA)
        };
        let accent = layer.resolve(Role::Accent);
        Palette {
            dim,
            card: layer.resolve(Role::Card),
            border: layer.resolve(Role::Border),
            accent,
            accent_soft: Color {
                a: accent.a * ACCENT_SOFT_ALPHA,
                ..accent
            },
            win: layer.resolve(Role::Window),
            win_float: layer.resolve(Role::Floating),
        }
    }
}

/// Where the base colours come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeSource {
    /// The built-in palette.
    Builtin,
    /// The colours of the active GTK theme.
    Gtk,
}

impl ThemeSource {
    fn parse(value: &str) -> Result<ThemeSource, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "gtk" => Ok(ThemeSource::Gtk),
            "default" | "builtin" => Ok(ThemeSource::Builtin),
            other => Err(format!(
                "unknown theme `{other}` (expected `gtk` or `default`)"
            )),
        }
    }
}

/// Which config file to read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ConfigChoice {
    /// `$XDG_CONFIG_HOME/swayflow/overview.conf`, if it exists.
    #[default]
    Auto,
    /// A file given with `--config`; it must be readable.
    File(PathBuf),
    /// `--no-config`.
    Disabled,
}

/// Parsed `swayflow overview` command line.
#[derive(Debug, Default)]
pub struct Options {
    pub theme: Option<ThemeSource>,
    pub colors: ColorSet,
    pub config: ConfigChoice,
    pub help: bool,
}

fn option_value(
    flag: &str,
    inline: Option<&str>,
    iter: &mut slice::Iter<'_, String>,
) -> Result<String, String> {
    if let Some(value) = inline {
        return Ok(value.to_owned());
    }
    match iter.next() {
        Some(value) if !value.starts_with("--") => Ok(value.clone()),
        _ => Err(format!("option `{flag}` needs a value")),
    }
}

/// Parses the arguments that follow `overview`. Both `--accent blue` and
/// `--accent=blue` are accepted.
pub fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value)),
            _ => (arg.as_str(), None),
        };
        match flag {
            "-h" | "--help" => options.help = true,
            "--no-config" => {
                if inline.is_some() {
                    return Err(format!("option `{flag}` takes no value"));
                }
                options.config = ConfigChoice::Disabled;
            }
            "--config" => {
                let value = option_value(flag, inline, &mut iter)?;
                options.config = ConfigChoice::File(PathBuf::from(value));
            }
            "--theme" => {
                let value = option_value(flag, inline, &mut iter)?;
                let theme = ThemeSource::parse(&value).map_err(|e| format!("{flag}: {e}"))?;
                options.theme = Some(theme);
            }
            _ => match flag.strip_prefix("--").and_then(Role::from_key) {
                Some(role) => {
                    let value = option_value(flag, inline, &mut iter)?;
                    let color = Color::parse(&value).map_err(|e| format!("{flag}: {e}"))?;
                    options.colors.set(role, color);
                }
                None => return Err(format!("unknown option `{arg}`")),
            },
        }
    }
    options.colors.derive_surfaces(None);
    Ok(options)
}

/// Contents of `overview.conf`.
#[derive(Debug, Default)]
pub struct Config {
    pub theme: Option<ThemeSource>,
    pub colors: ColorSet,
    /// Problems found while parsing; bad lines are skipped, not fatal.
    pub warnings: Vec<String>,
}

/// Cuts a trailing comment (`# ...` or `; ...` after whitespace) off a value.
fn strip_inline_comment(value: &str) -> &str {
    let bytes = value.as_bytes();
    for i in 1..bytes.len() {
        if (bytes[i] == b'#' || bytes[i] == b';') && bytes[i - 1].is_ascii_whitespace() {
            return value[..i].trim_end();
        }
    }
    value
}

/// Removes one pair of matching surrounding quotes.
fn unquote(value: &str) -> &str {
    let value = value.trim();
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner.trim();
        }
    }
    value
}

/// Parses the config file: `key = value` lines, `#` / `;` comment lines.
/// Keys are `theme` and the colour roles.
pub fn parse_config(text: &str) -> Config {
    let mut config = Config::default();
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        let line = raw.trim();
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with(';')
            || (line.starts_with('[') && line.ends_with(']'))
        {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            config
                .warnings
                .push(format!("line {number}: expected `key = value`"));
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = unquote(strip_inline_comment(value.trim()));
        if key == "theme" {
            match ThemeSource::parse(value) {
                Ok(theme) => config.theme = Some(theme),
                Err(e) => config.warnings.push(format!("line {number}: theme: {e}")),
            }
        } else if let Some(role) = Role::from_key(&key) {
            match Color::parse(value) {
                Ok(color) => config.colors.set(role, color),
                Err(e) => config.warnings.push(format!("line {number}: {key}: {e}")),
            }
        } else {
            config
                .warnings
                .push(format!("line {number}: unknown key `{key}`"));
        }
    }
    config.colors.derive_surfaces(None);
    config
}

/// Reads the config file. A missing default file is not an error; a file named
/// with `--config` must exist. Unreadable default files and bad lines only
/// produce warnings.
pub fn load_config(choice: &ConfigChoice) -> Result<Config, String> {
    let (path, explicit) = match choice {
        ConfigChoice::Disabled => return Ok(Config::default()),
        ConfigChoice::File(path) => (path.clone(), true),
        ConfigChoice::Auto => match overview_config() {
            Some(path) => (path, false),
            None => return Ok(Config::default()),
        },
    };
    match fs::read_to_string(&path) {
        Ok(text) => {
            let mut config = parse_config(&text);
            let shown = path.display();
            for warning in &mut config.warnings {
                *warning = format!("{shown}: {warning}");
            }
            Ok(config)
        }
        Err(e) if !explicit && e.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) if explicit => Err(format!("cannot read {}: {e}", path.display())),
        Err(e) => {
            let mut config = Config::default();
            config
                .warnings
                .push(format!("cannot read {}: {e}", path.display()));
            Ok(config)
        }
    }
}

/// Everything the UI needs to know about colours.
#[derive(Clone, Debug)]
pub struct Style {
    /// Config file merged with the command line (the command line wins).
    pub overrides: ColorSet,
    /// Whether the colours of the GTK theme form the base layer.
    pub use_gtk: bool,
}

/// Combines the command line and the config file.
pub fn build_style(options: &Options, config: &Config) -> Style {
    let mut overrides = ColorSet::default();
    overrides.overlay(&config.colors);
    overrides.overlay(&options.colors);
    Style {
        overrides,
        use_gtk: options.theme.or(config.theme) == Some(ThemeSource::Gtk),
    }
}

impl Style {
    /// Final palette; `gtk` is the layer read from the GTK theme (if requested
    /// and available) and sits below the config file and the command line.
    pub fn palette(&self, gtk: Option<&ColorSet>) -> Palette {
        let mut merged = ColorSet::default();
        if let Some(gtk) = gtk {
            merged.overlay(gtk);
        }
        merged.overlay(&self.overrides);
        Palette::from_layer(&merged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn rgb(c: Color) -> (u8, u8, u8) {
        (
            (c.r * 255.0).round() as u8,
            (c.g * 255.0).round() as u8,
            (c.b * 255.0).round() as u8,
        )
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn color(text: &str) -> Color {
        Color::parse(text).expect("valid colour")
    }

    #[test]
    fn built_in_palette_keeps_the_original_look() {
        let p = Palette::from_layer(&ColorSet::default());
        assert!(close(p.dim.r, 11.0 / 255.0) && close(p.dim.a, 0.80));
        assert!(close(p.card.a, 0.96));
        assert_eq!(rgb(p.card), (0x0f, 0x12, 0x17));
        assert_eq!(rgb(p.border), (0x2e, 0x38, 0x42));
        assert_eq!(rgb(p.accent), (0x88, 0xc0, 0xd0));
        assert_eq!(rgb(p.win), (0x1b, 0x20, 0x27));
        assert_eq!(rgb(p.win_float), (0x26, 0x2c, 0x32));
        assert!(close(p.accent_soft.a, 0.22));
    }

    #[test]
    fn background_alpha_is_only_defaulted_when_not_given() {
        let mut layer = ColorSet::default();
        layer.set(Role::Background, color("#112233"));
        assert!(close(Palette::from_layer(&layer).dim.a, 0.80));
        layer.set(Role::Background, color("rgba(17, 34, 51, 0.5)"));
        assert!(close(Palette::from_layer(&layer).dim.a, 0.5));
    }

    #[test]
    fn surfaces_are_derived_from_a_dark_background() {
        let mut layer = ColorSet::default();
        layer.set(Role::Background, color("#000000"));
        layer.derive_surfaces(None);
        let p = Palette::from_layer(&layer);
        assert!(p.win.luminance() > 0.0);
        assert!(p.win_float.luminance() > p.win.luminance());
        assert!(p.border.luminance() > p.win_float.luminance());
        // Roles the layer has no opinion about stay built-in.
        assert_eq!(rgb(p.accent), (0x88, 0xc0, 0xd0));
    }

    #[test]
    fn surfaces_are_derived_from_a_light_background() {
        let mut layer = ColorSet::default();
        layer.set(Role::Background, color("white"));
        layer.derive_surfaces(None);
        let p = Palette::from_layer(&layer);
        assert!(p.win.luminance() < 1.0);
        assert!(p.win_float.luminance() < p.win.luminance());
        assert!(p.border.luminance() < p.win_float.luminance());
    }

    #[test]
    fn derivation_keeps_explicit_surfaces() {
        let mut layer = ColorSet::default();
        layer.set(Role::Background, color("#000000"));
        layer.set(Role::Window, color("#123456"));
        layer.derive_surfaces(None);
        assert_eq!(rgb(layer.get(Role::Window).unwrap()), (0x12, 0x34, 0x56));
        assert!(layer.get(Role::Card).is_some());
    }

    #[test]
    fn derivation_needs_a_background() {
        let mut layer = ColorSet::default();
        layer.set(Role::Accent, color("red"));
        layer.derive_surfaces(None);
        assert!(layer.get(Role::Window).is_none());
        assert!(!layer.is_empty());
        assert!(ColorSet::default().is_empty());
    }

    #[test]
    fn parses_command_line() {
        let o = parse_args(&args(&[
            "--accent",
            "blue",
            "--background=#000000",
            "--theme",
            "gtk",
            "--no-config",
        ]))
        .unwrap();
        assert_eq!(o.theme, Some(ThemeSource::Gtk));
        assert_eq!(o.config, ConfigChoice::Disabled);
        assert_eq!(rgb(o.colors.get(Role::Accent).unwrap()), (0, 0, 255));
        assert!(o.colors.get(Role::Window).is_some(), "derived from --background");
        assert!(!o.help);

        let o = parse_args(&args(&["--config", "/tmp/x.conf", "-h"])).unwrap();
        assert_eq!(o.config, ConfigChoice::File(PathBuf::from("/tmp/x.conf")));
        assert!(o.help);

        let o = parse_args(&[]).unwrap();
        assert_eq!(o.config, ConfigChoice::Auto);
        assert!(o.colors.is_empty() && o.theme.is_none());
    }

    #[test]
    fn rejects_bad_command_lines() {
        let cases: [&[&str]; 8] = [
            &["--accent"],
            &["--accent", "--card"],
            &["--accent", "bleu"],
            &["--bogus"],
            &["overview"],
            &["--theme", "qt"],
            &["--no-config=1"],
            &["--config"],
        ];
        for list in cases {
            assert!(parse_args(&args(list)).is_err(), "{list:?} must be rejected");
        }
    }

    #[test]
    fn parses_config_file() {
        let text = "# comment\n; also a comment\n\n[overview]\ntheme = gtk\n\
                    accent = #ff0000   # red\nBackground = \"#101010\"\n\
                    window = rgba(1, 2, 3, 0.5)\nfloating=\nbogus = 1\nnoequals\ncard = nope\n";
        let config = parse_config(text);
        assert_eq!(config.theme, Some(ThemeSource::Gtk));
        assert_eq!(rgb(config.colors.get(Role::Accent).unwrap()), (255, 0, 0));
        assert_eq!(rgb(config.colors.get(Role::Background).unwrap()), (16, 16, 16));
        let window = config.colors.get(Role::Window).unwrap();
        assert!(window.has_alpha && close(window.a, 0.5));
        // card and floating were invalid, but derivation still fills them in.
        assert!(config.colors.get(Role::Card).is_some());
        assert_eq!(config.warnings.len(), 4, "{:?}", config.warnings);
    }

    #[test]
    fn comment_stripping_leaves_colours_alone() {
        assert_eq!(strip_inline_comment("#ff0000"), "#ff0000");
        assert_eq!(strip_inline_comment("#ff0000 # red"), "#ff0000");
        assert_eq!(strip_inline_comment("rgb(1, 2, 3) ; blue-ish"), "rgb(1, 2, 3)");
        assert_eq!(strip_inline_comment(""), "");
        assert_eq!(unquote("'#abc'"), "#abc");
        assert_eq!(unquote("\"steelblue\""), "steelblue");
        assert_eq!(unquote("plain"), "plain");
    }

    #[test]
    fn command_line_beats_config_and_config_beats_gtk() {
        let options = parse_args(&args(&["--theme", "default", "--accent", "red"])).unwrap();
        let mut config = Config::default();
        config.theme = Some(ThemeSource::Gtk);
        config.colors.set(Role::Accent, color("blue"));
        config.colors.set(Role::Border, color("#010203"));

        let style = build_style(&options, &config);
        assert!(!style.use_gtk, "--theme on the command line beats the config");
        let p = style.palette(None);
        assert_eq!(rgb(p.accent), (255, 0, 0));
        assert_eq!(rgb(p.border), (1, 2, 3));

        let options = parse_args(&[]).unwrap();
        assert!(build_style(&options, &config).use_gtk);
    }

    #[test]
    fn gtk_layer_sits_below_overrides() {
        let mut gtk = ColorSet::default();
        gtk.set(Role::Background, color("#202020"));
        gtk.set(Role::Accent, color("#00ff00"));
        let mut overrides = ColorSet::default();
        overrides.set(Role::Accent, color("#0000ff"));
        let style = Style {
            overrides,
            use_gtk: true,
        };

        let p = style.palette(Some(&gtk));
        assert_eq!(rgb(p.accent), (0, 0, 255));
        assert!(close(p.dim.r, 32.0 / 255.0));

        let p = style.palette(None);
        assert!(close(p.dim.r, 11.0 / 255.0));
    }
}
