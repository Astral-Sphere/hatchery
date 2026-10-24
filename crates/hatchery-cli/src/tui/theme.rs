//! The theme: one semantic palette, two flavours (dark and light terminals).
//!
//! Every widget asks the theme for a *role* (accent, dim, error, …), never for a colour, so the
//! two palettes below are the only place colours exist. `auto` picks a flavour from the
//! terminal's own answer to an OSC 11 background query (the same probe codex runs), falling back
//! to `$COLORFGBG` and then to dark — a wrong guess is a preference, never an error.

use ratatui::style::Color;

/// Which palette is in effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeName {
    /// For dark terminal backgrounds.
    Dark,
    /// For light terminal backgrounds.
    Light,
}

/// What the user asked for (`ui.theme` / `/theme`); `Auto` defers to the terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeSetting {
    /// Follow the terminal background.
    Auto,
    /// Force [`ThemeName::Dark`].
    Dark,
    /// Force [`ThemeName::Light`].
    Light,
}

impl ThemeSetting {
    /// Parses the config / command spelling; anything else is `None` (a usage reply).
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }

    /// The spelling the status bar and usage replies show.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }
}

/// The semantic palette every widget renders through.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    /// Which palette this is.
    pub name: ThemeName,
    /// User and assistant glyphs, headings, the prompt glyph, the spinner.
    pub accent: Color,
    /// Body text.
    pub text: Color,
    /// Secondary text: tool titles, details, quotes.
    pub dim: Color,
    /// Hints, fold lines, borders, rules.
    pub faint: Color,
    /// Completed tool calls, ok toasts.
    pub success: Color,
    /// Approval waits, rate limits, denials.
    pub warn: Color,
    /// Failures and error toasts.
    pub error: Color,
    /// Code bodies and the left bar of code and tool cells.
    pub code: Color,
    /// Tool names.
    pub tool: Color,
    /// Reasoning bodies.
    pub thinking: Color,
}

impl Theme {
    /// The palette for dark backgrounds.
    #[must_use]
    pub const fn dark() -> Self {
        Self {
            name: ThemeName::Dark,
            accent: Color::Rgb(178, 140, 255),
            text: Color::Rgb(222, 222, 228),
            dim: Color::Rgb(148, 148, 160),
            faint: Color::Rgb(104, 104, 118),
            success: Color::Rgb(116, 198, 140),
            warn: Color::Rgb(226, 178, 84),
            error: Color::Rgb(228, 112, 112),
            code: Color::Rgb(168, 168, 182),
            tool: Color::Rgb(122, 178, 255),
            thinking: Color::Rgb(128, 128, 144),
        }
    }

    /// The palette for light backgrounds.
    #[must_use]
    pub const fn light() -> Self {
        Self {
            name: ThemeName::Light,
            accent: Color::Rgb(112, 72, 198),
            text: Color::Rgb(32, 32, 38),
            dim: Color::Rgb(98, 98, 110),
            faint: Color::Rgb(142, 142, 152),
            success: Color::Rgb(38, 138, 72),
            warn: Color::Rgb(164, 106, 0),
            error: Color::Rgb(196, 58, 58),
            code: Color::Rgb(88, 88, 102),
            tool: Color::Rgb(28, 98, 188),
            thinking: Color::Rgb(118, 118, 132),
        }
    }

    /// The palette for a flavour.
    #[must_use]
    pub const fn of(name: ThemeName) -> Self {
        match name {
            ThemeName::Dark => Self::dark(),
            ThemeName::Light => Self::light(),
        }
    }
}

/// Resolves a setting into a palette. `probe` is the OSC 11 answer, `colorfgbg` the environment
/// variable of the same name; both are `None` on terminals that stay silent.
#[must_use]
pub fn resolve(
    setting: ThemeSetting,
    probe: Option<(u8, u8, u8)>,
    colorfgbg: Option<&str>,
) -> Theme {
    let name = match setting {
        ThemeSetting::Dark => ThemeName::Dark,
        ThemeSetting::Light => ThemeName::Light,
        ThemeSetting::Auto => probe
            .map(|rgb| {
                if is_dark(rgb) {
                    ThemeName::Dark
                } else {
                    ThemeName::Light
                }
            })
            .or_else(|| {
                colorfgbg.and_then(colorfgbg_is_dark).map(|dark| {
                    if dark {
                        ThemeName::Dark
                    } else {
                        ThemeName::Light
                    }
                })
            })
            .unwrap_or(ThemeName::Dark),
    };
    Theme::of(name)
}

/// Relative luminance below the middle grey counts as a dark background.
#[must_use]
pub fn is_dark((r, g, b): (u8, u8, u8)) -> bool {
    (0.2126 * f64::from(r) + 0.7152 * f64::from(g) + 0.0722 * f64::from(b)) / 255.0 < 0.5
}

/// `$COLORFGBG` is `fg;bg` (or `fg;default;bg`) in ANSI palette indices; only the 16 basic
/// colours say anything about brightness, so anything else yields `None`.
#[must_use]
pub fn colorfgbg_is_dark(value: &str) -> Option<bool> {
    let background = value.split(';').next_back()?.trim().parse::<u8>().ok()?;
    match background {
        0..=6 | 8 => Some(true),
        7 | 9..=15 => Some(false),
        _ => None,
    }
}

/// Parses a terminal's OSC 11 reply, e.g. `\x1b]11;rgb:1e1e/2020/2828\x1b\\` (or BEL-terminated,
/// or `rgba:` with an alpha channel). Each channel carries one to four hex digits; shorter
/// forms are scaled up, so `rgb:f/f/f` is white.
#[must_use]
pub fn parse_osc11_rgb(bytes: &[u8]) -> Option<(u8, u8, u8)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let body = text.split("]11;").nth(1)?;
    let body = body
        .strip_prefix("rgb:")
        .or_else(|| body.strip_prefix("rgba:"))?;
    // The reply ends in BEL or ST; without one the reply was cut off and says nothing.
    let body = body
        .strip_suffix('\u{7}')
        .or_else(|| body.strip_suffix("\u{1b}\\"))?;
    let mut channels = body.split('/');
    let r = scale_hex(channels.next()?)?;
    let g = scale_hex(channels.next()?)?;
    let b = scale_hex(channels.next()?)?;
    Some((r, g, b))
}

/// One to four hex digits into a byte, scaling short forms to the full range.
fn scale_hex(field: &str) -> Option<u8> {
    let value = u16::from_str_radix(field, 16).ok()?;
    Some(match field.len() {
        1 => u8::try_from(value * 17).ok()?,
        2 => u8::try_from(value).ok()?,
        3 => u8::try_from(value >> 4).ok()?,
        4 => u8::try_from(value >> 8).ok()?,
        _ => return None,
    })
}

/// Asks the controlling terminal for its background colour, waiting at most 150 ms.
///
/// Runs once at startup, after raw mode is on and before the event stream exists, so the probe
/// thread is the only reader of the tty for its brief window. A terminal that never answers
/// costs one startup delay of 150 ms and yields `None`; the reader thread then stays blocked on
/// the tty until the process exits, which is cheaper than piping the reply through the event
/// stream's parser.
#[must_use]
pub fn detect_background() -> Option<(u8, u8, u8)> {
    detect_background_with(std::time::Duration::from_millis(150))
}

fn detect_background_with(timeout: std::time::Duration) -> Option<(u8, u8, u8)> {
    #[cfg(not(unix))]
    {
        let _ = timeout;
        None
    }
    #[cfg(unix)]
    {
        use std::io::{Read, Write};
        let mut tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .ok()?;
        let mut reader = tty.try_clone().ok()?;
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("osc11-probe".to_owned())
            .spawn(move || {
                let mut buffer = [0u8; 96];
                if let Ok(read) = reader.read(&mut buffer) {
                    let _ = sender.send(buffer[..read].to_vec());
                }
            })
            .ok()?;
        tty.write_all(b"\x1b]11;?\x1b\\").ok()?;
        let bytes = receiver.recv_timeout(timeout).ok()?;
        parse_osc11_rgb(&bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_palettes_disagree_on_every_role() {
        let (dark, light) = (Theme::dark(), Theme::light());
        assert_eq!(dark.name, ThemeName::Dark);
        assert_eq!(light.name, ThemeName::Light);
        for (a, b) in [
            (dark.accent, light.accent),
            (dark.text, light.text),
            (dark.dim, light.dim),
            (dark.faint, light.faint),
            (dark.success, light.success),
            (dark.warn, light.warn),
            (dark.error, light.error),
            (dark.code, light.code),
            (dark.tool, light.tool),
            (dark.thinking, light.thinking),
        ] {
            assert_ne!(a, b, "a role must differ between flavours");
        }
    }

    #[test]
    fn osc11_replies_parse_in_every_hex_width() {
        assert_eq!(
            parse_osc11_rgb(b"\x1b]11;rgb:1e1e/2020/2828\x1b\\"),
            Some((0x1e, 0x20, 0x28))
        );
        assert_eq!(
            parse_osc11_rgb(b"\x1b]11;rgb:ffff/ffff/ffff\x07"),
            Some((255, 255, 255))
        );
        assert_eq!(
            parse_osc11_rgb(b"\x1b]11;rgb:f/f/f\x07"),
            Some((255, 255, 255))
        );
        assert_eq!(
            parse_osc11_rgb(b"\x1b]11;rgb:0000/0000/0000\x07"),
            Some((0, 0, 0))
        );
        assert_eq!(
            parse_osc11_rgb(b"\x1b]11;rgba:ffff/eeee/dddd/ffff\x1b\\"),
            Some((255, 0xee, 0xdd)),
            "alpha rides along and is ignored"
        );
    }

    #[test]
    fn truncated_and_foreign_replies_parse_to_nothing() {
        assert_eq!(
            parse_osc11_rgb(b"\x1b]11;rgb:ffff/ffff"),
            None,
            "no terminator"
        );
        assert_eq!(
            parse_osc11_rgb(b"\x1b]10;rgb:ffff/ffff/ffff\x07"),
            None,
            "OSC 10 is fg"
        );
        assert_eq!(parse_osc11_rgb(b"typed text"), None);
        assert_eq!(parse_osc11_rgb(b"\x1b]11;not-a-color\x07"), None);
    }

    #[test]
    fn luminance_splits_the_greys() {
        assert!(is_dark((0x1e, 0x20, 0x28)));
        assert!(is_dark((0x55, 0x57, 0x53)), "terminal grey is dark");
        assert!(!is_dark((0xee, 0xee, 0xec)));
        assert!(!is_dark((255, 255, 255)));
    }

    #[test]
    fn colorfgbg_reads_the_background_half() {
        assert_eq!(colorfgbg_is_dark("15;0"), Some(true));
        assert_eq!(colorfgbg_is_dark("0;15"), Some(false));
        assert_eq!(colorfgbg_is_dark("15;default;0"), Some(true));
        assert_eq!(
            colorfgbg_is_dark("15;258"),
            None,
            "outside the 16 basic colours"
        );
        assert_eq!(colorfgbg_is_dark(""), None);
    }

    #[test]
    fn resolve_prefers_the_probe_then_the_environment_then_dark() {
        assert_eq!(
            resolve(ThemeSetting::Auto, Some((0x1e, 0x20, 0x28)), Some("15;0")).name,
            ThemeName::Dark
        );
        assert_eq!(
            resolve(ThemeSetting::Auto, Some((255, 255, 255)), Some("15;0")).name,
            ThemeName::Light,
            "the probe beats the environment"
        );
        assert_eq!(
            resolve(ThemeSetting::Auto, None, Some("0;15")).name,
            ThemeName::Light
        );
        assert_eq!(
            resolve(ThemeSetting::Auto, None, None).name,
            ThemeName::Dark
        );
        assert_eq!(
            resolve(ThemeSetting::Light, Some((0, 0, 0)), None).name,
            ThemeName::Light
        );
    }

    #[test]
    fn settings_parse_case_insensitively_and_reject_nonsense() {
        assert_eq!(ThemeSetting::parse("Dark"), Some(ThemeSetting::Dark));
        assert_eq!(ThemeSetting::parse("auto"), Some(ThemeSetting::Auto));
        assert_eq!(ThemeSetting::parse("banana"), None);
        assert_eq!(ThemeSetting::Auto.as_str(), "auto");
    }

    #[test]
    fn a_silent_terminal_costs_only_the_deadline() {
        // /dev/tty under a test harness is not an answering terminal; the probe must give up
        // quietly rather than hang or panic.
        let started = std::time::Instant::now();
        let _ = detect_background_with(std::time::Duration::from_millis(50));
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
    }
}
