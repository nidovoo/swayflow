//! Colour values and text parsing for the overview palette.
//!
//! Independent of GTK, so it also builds (and is unit-tested) without the
//! `overview` feature.
#![cfg_attr(not(feature = "overview"), allow(dead_code))]

/// An sRGB colour with straight (non-premultiplied) alpha; channels are `0.0..=1.0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
    /// Whether the colour was given together with an explicit alpha channel
    /// (`#rrggbbaa`, `rgba(...)`, `transparent`, ...).
    pub has_alpha: bool,
}

/// Standard CSS colour names (a practical subset).
const NAMED: &[(&str, u32)] = &[
    ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4),
    ("beige", 0xf5f5dc),
    ("black", 0x000000),
    ("blue", 0x0000ff),
    ("brown", 0xa52a2a),
    ("chocolate", 0xd2691e),
    ("coral", 0xff7f50),
    ("cornflowerblue", 0x6495ed),
    ("crimson", 0xdc143c),
    ("cyan", 0x00ffff),
    ("darkblue", 0x00008b),
    ("darkcyan", 0x008b8b),
    ("darkgray", 0xa9a9a9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xa9a9a9),
    ("darkorange", 0xff8c00),
    ("darkred", 0x8b0000),
    ("deeppink", 0xff1493),
    ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222),
    ("forestgreen", 0x228b22),
    ("fuchsia", 0xff00ff),
    ("gainsboro", 0xdcdcdc),
    ("gold", 0xffd700),
    ("goldenrod", 0xdaa520),
    ("gray", 0x808080),
    ("green", 0x008000),
    ("grey", 0x808080),
    ("hotpink", 0xff69b4),
    ("indigo", 0x4b0082),
    ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c),
    ("lavender", 0xe6e6fa),
    ("lightblue", 0xadd8e6),
    ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90),
    ("lightgrey", 0xd3d3d3),
    ("lightyellow", 0xffffe0),
    ("lime", 0x00ff00),
    ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6),
    ("magenta", 0xff00ff),
    ("maroon", 0x800000),
    ("midnightblue", 0x191970),
    ("mintcream", 0xf5fffa),
    ("navy", 0x000080),
    ("olive", 0x808000),
    ("orange", 0xffa500),
    ("orangered", 0xff4500),
    ("orchid", 0xda70d6),
    ("peru", 0xcd853f),
    ("pink", 0xffc0cb),
    ("plum", 0xdda0dd),
    ("purple", 0x800080),
    ("rebeccapurple", 0x663399),
    ("red", 0xff0000),
    ("royalblue", 0x4169e1),
    ("salmon", 0xfa8072),
    ("seagreen", 0x2e8b57),
    ("sienna", 0xa0522d),
    ("silver", 0xc0c0c0),
    ("skyblue", 0x87ceeb),
    ("slategray", 0x708090),
    ("slategrey", 0x708090),
    ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f),
    ("steelblue", 0x4682b4),
    ("tan", 0xd2b48c),
    ("teal", 0x008080),
    ("tomato", 0xff6347),
    ("turquoise", 0x40e0d0),
    ("violet", 0xee82ee),
    ("wheat", 0xf5deb3),
    ("white", 0xffffff),
    ("whitesmoke", 0xf5f5f5),
    ("yellow", 0xffff00),
];

impl Color {
    /// Opaque colour from channel values in `0.0..=1.0`.
    pub fn rgb(r: f64, g: f64, b: f64) -> Color {
        Color {
            r,
            g,
            b,
            a: 1.0,
            has_alpha: false,
        }
    }

    /// Opaque colour from `0xrrggbb`.
    pub fn from_hex(value: u32) -> Color {
        Color::rgb(
            f64::from((value >> 16) & 0xff) / 255.0,
            f64::from((value >> 8) & 0xff) / 255.0,
            f64::from(value & 0xff) / 255.0,
        )
    }

    fn from_bytes(r: u8, g: u8, b: u8) -> Color {
        Color::rgb(
            f64::from(r) / 255.0,
            f64::from(g) / 255.0,
            f64::from(b) / 255.0,
        )
    }

    /// The same colour with an explicit alpha (clamped to `0.0..=1.0`).
    pub fn with_alpha(self, a: f64) -> Color {
        Color {
            a: a.clamp(0.0, 1.0),
            has_alpha: true,
            ..self
        }
    }

    /// Perceived brightness, `0.0` (black) ..= `1.0` (white).
    pub fn luminance(self) -> f64 {
        0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b
    }

    /// Linear blend towards `other` (`t = 0` keeps `self`, `t = 1` gives `other`);
    /// the alpha of `self` is kept.
    pub fn mix(self, other: Color, t: f64) -> Color {
        let t = t.clamp(0.0, 1.0);
        Color {
            r: self.r + (other.r - self.r) * t,
            g: self.g + (other.g - self.g) * t,
            b: self.b + (other.b - self.b) * t,
            ..self
        }
    }

    /// Parses a colour: a CSS name (`steelblue`), `#rgb`, `#rgba`, `#rrggbb`,
    /// `#rrggbbaa` (the `#` may be omitted), `rgb(r, g, b)` or `rgba(r, g, b, a)`.
    /// Channels are `0..255` or percentages, alpha is `0..1` or a percentage.
    pub fn parse(text: &str) -> Result<Color, String> {
        let t = text.trim();
        if t.is_empty() {
            return Err("empty colour value".to_owned());
        }
        if let Some(hex) = t.strip_prefix('#') {
            return parse_hex(hex).ok_or_else(|| format!("invalid hex colour `{t}`"));
        }
        let lower = t.to_ascii_lowercase();
        if let Some(args) = func_args(&lower, "rgba").or_else(|| func_args(&lower, "rgb")) {
            return parse_func(args, t);
        }
        if lower == "transparent" {
            return Ok(Color::rgb(0.0, 0.0, 0.0).with_alpha(0.0));
        }
        if let Some(entry) = NAMED.iter().find(|entry| entry.0 == lower.as_str()) {
            return Ok(Color::from_hex(entry.1));
        }
        if let Some(color) = parse_hex(&lower) {
            return Ok(color);
        }
        Err(format!("unknown colour `{t}`"))
    }
}

/// Digits after `#`: 3, 4, 6 or 8 hexadecimal characters.
fn parse_hex(hex: &str) -> Option<Color> {
    if !hex.is_ascii() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |s: &str| u8::from_str_radix(s, 16).ok();
    let (r, g, b, a) = match hex.len() {
        3 | 4 => {
            let d = |i: usize| byte(&hex[i..=i]).map(|v| v * 17);
            let a = if hex.len() == 4 { Some(d(3)?) } else { None };
            (d(0)?, d(1)?, d(2)?, a)
        }
        6 | 8 => {
            let p = |i: usize| byte(&hex[i..i + 2]);
            let a = if hex.len() == 8 { Some(p(6)?) } else { None };
            (p(0)?, p(2)?, p(4)?, a)
        }
        _ => return None,
    };
    let color = Color::from_bytes(r, g, b);
    Some(match a {
        Some(a) => color.with_alpha(f64::from(a) / 255.0),
        None => color,
    })
}

/// The text between the parentheses of `name( ... )`.
fn func_args<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let rest = text.strip_prefix(name)?.trim_start();
    let inner = rest.strip_prefix('(')?;
    inner.strip_suffix(')')
}

fn parse_func(args: &str, original: &str) -> Result<Color, String> {
    let bad = || format!("invalid colour `{original}`");
    let cleaned = args.replace(',', " ").replace('/', " ");
    let parts: Vec<&str> = cleaned.split_whitespace().collect();
    if parts.len() != 3 && parts.len() != 4 {
        return Err(bad());
    }
    let channel = |s: &str| -> Option<f64> {
        let value = match s.strip_suffix('%') {
            Some(percent) => percent.parse::<f64>().ok().map(|v| v / 100.0),
            None => s.parse::<f64>().ok().map(|v| v / 255.0),
        };
        value.map(|v| v.clamp(0.0, 1.0))
    };
    let alpha = |s: &str| -> Option<f64> {
        let value = match s.strip_suffix('%') {
            Some(percent) => percent.parse::<f64>().ok().map(|v| v / 100.0),
            None => s.parse::<f64>().ok(),
        };
        value.map(|v| v.clamp(0.0, 1.0))
    };
    let r = channel(parts[0]).ok_or_else(bad)?;
    let g = channel(parts[1]).ok_or_else(bad)?;
    let b = channel(parts[2]).ok_or_else(bad)?;
    let color = Color::rgb(r, g, b);
    if parts.len() == 4 {
        let a = alpha(parts[3]).ok_or_else(bad)?;
        Ok(color.with_alpha(a))
    } else {
        Ok(color)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn bytes(c: Color) -> (u8, u8, u8) {
        (
            (c.r * 255.0).round() as u8,
            (c.g * 255.0).round() as u8,
            (c.b * 255.0).round() as u8,
        )
    }

    #[test]
    fn parses_hex_forms() {
        assert_eq!(bytes(Color::parse("#0b0e13").unwrap()), (11, 14, 19));
        assert_eq!(bytes(Color::parse("#fff").unwrap()), (255, 255, 255));
        assert_eq!(bytes(Color::parse("88C0D0").unwrap()), (136, 192, 208));
        assert!(!Color::parse("#0b0e13").unwrap().has_alpha);

        let c = Color::parse("#00000080").unwrap();
        assert!(c.has_alpha);
        assert!(close(c.a, 128.0 / 255.0));

        let c = Color::parse("#f008").unwrap();
        assert_eq!(bytes(c), (255, 0, 0));
        assert!(close(c.a, 136.0 / 255.0));
    }

    #[test]
    fn parses_names_case_insensitively() {
        assert_eq!(bytes(Color::parse("Blue").unwrap()), (0, 0, 255));
        assert_eq!(bytes(Color::parse(" steelblue ").unwrap()), (70, 130, 180));
        let t = Color::parse("transparent").unwrap();
        assert!(t.has_alpha && close(t.a, 0.0));
    }

    #[test]
    fn parses_functional_notation() {
        let c = Color::parse("rgb(11, 14, 19)").unwrap();
        assert_eq!(bytes(c), (11, 14, 19));
        assert!(!c.has_alpha);

        let c = Color::parse("rgba(0,0,0,0.5)").unwrap();
        assert!(c.has_alpha && close(c.a, 0.5));

        let c = Color::parse("RGB(0 128 255 / 50%)").unwrap();
        assert_eq!(bytes(c), (0, 128, 255));
        assert!(close(c.a, 0.5));

        assert_eq!(bytes(Color::parse("rgb(100%, 0%, 0%)").unwrap()), (255, 0, 0));
        // Out-of-range channels are clamped.
        assert_eq!(bytes(Color::parse("rgb(300, -5, 0)").unwrap()), (255, 0, 0));
    }

    #[test]
    fn rejects_garbage() {
        for text in ["", "   ", "#12", "#gggggg", "#12345", "bleu", "rgb(1,2)", "rgb(a,b,c)", "rgb(1,2,3"] {
            assert!(Color::parse(text).is_err(), "`{text}` must be rejected");
        }
    }

    #[test]
    fn mixes_and_measures_brightness() {
        let black = Color::rgb(0.0, 0.0, 0.0);
        let white = Color::rgb(1.0, 1.0, 1.0);
        let m = black.mix(white, 0.25);
        assert!(close(m.r, 0.25) && close(m.g, 0.25) && close(m.b, 0.25));
        assert!(close(black.luminance(), 0.0));
        assert!(close(white.luminance(), 1.0));

        let kept = black.with_alpha(0.4).mix(white, 1.0);
        assert!(close(kept.a, 0.4) && kept.has_alpha);
    }
}
