/// Dynamic color scheme detection for ccat.
///
/// Auto-detects terminal background (dark/light) and provides
/// appropriate ANSI style strings for tree display and syntect
/// theme selection for source highlighting.
///
/// Detection priority:
/// 1. `CCAT_COLOR_SCHEME=dark|light|auto` env var
/// 2. `--color-scheme` CLI flag (set via [`force_theme`])
/// 3. `COLORFGBG` env var (rxvt, some terminals)
/// 4. OSC 11 query (modern terminals, best effort)
/// 5. Default: Dark

use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Theme detection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Theme {
    Dark,
    Light,
}

/// Stores the forced theme override (if any). `None` = auto-detect on first use.
static FORCED: std::sync::Mutex<Option<Theme>> = std::sync::Mutex::new(None);

/// Stores the auto-detected theme (initialised lazily). This allows tests to
/// reset state by calling `force_theme` again even after `active_theme` was called.
static AUTO_THEME: OnceLock<Theme> = OnceLock::new();

/// Override the detected theme (called from CLI arg parsing or tests).
/// Pass `None` to reset to auto-detection on next `active_theme()` call.
pub fn force_theme(t: Option<Theme>) {
    let resolved = t.unwrap_or_else(detect_from_env);
    let mut guard = FORCED.lock().unwrap();
    *guard = Some(resolved);
}

/// Get the currently active theme (initialises detection on first call).
pub fn active_theme() -> Theme {
    // Check forced override first
    {
        let guard = FORCED.lock().unwrap();
        if let Some(t) = *guard {
            return t;
        }
    }
    // Fall back to auto-detected (lazily initialised)
    *AUTO_THEME.get_or_init(detect_from_env)
}

/// Detect theme from `CCAT_COLOR_SCHEME` env var, or `COLORFGBG`, or OSC 11.
fn detect_from_env() -> Theme {
    // 1. Explicit env var
    if let Ok(val) = std::env::var("CCAT_COLOR_SCHEME") {
        match val.to_lowercase().as_str() {
            "dark" => return Theme::Dark,
            "light" => return Theme::Light,
            "auto" => {} // fall through to auto-detection
            _ => {}
        }
    }

    // 2. COLORFGBG — set by rxvt and some terminals
    // Format: "15;0" (fg=15=white, bg=0=black) or "0;15" (fg=black, bg=white)
    if let Ok(val) = std::env::var("COLORFGBG") {
        let parts: Vec<&str> = val.split(';').collect();
        if let Some(bg) = parts.last() {
            if let Ok(bg_idx) = bg.parse::<u8>() {
                // Light background colors are typically 7 (white) or 15 (bright white)
                if bg_idx == 7 || bg_idx == 15 || bg_idx >= 232 {
                    // Bright background (232-255 are grayscale, 255=white)
                    // But dark backgrounds could also be 0 (black) or 16-231
                    // Safer: check COLORFGBG format
                    // If fg;bg and bg > 7 → likely light
                    if bg_idx > 7 && bg_idx < 232 {
                        // Light background
                        return Theme::Light;
                    }
                }
            }
        }
    }

    // 3. Try OSC 11 query — best effort, non-blocking
    if let Some(theme) = detect_via_osc11() {
        return theme;
    }

    // 4. COLORTERM hint — truecolor terminals usually use dark themes
    if let Ok(val) = std::env::var("COLORTERM") {
        let v = val.to_lowercase();
        // light-terminal or similar
        if v.contains("light") {
            return Theme::Light;
        }
    }

    // 5. TERM_PROGRAM (macOS iTerm2, Terminal.app)
    if let Ok(profile) = std::env::var("ITERM_PROFILE") {
        let p = profile.to_lowercase();
        if p.contains("light") || p.contains("solarized light") {
            return Theme::Light;
        }
    }

    // 6. KDE Konsole
    if let Ok(profile) = std::env::var("KONSOLE_PROFILE_NAME") {
        let p = profile.to_lowercase();
        if p.contains("light") {
            return Theme::Light;
        }
    }

    // Default: dark
    Theme::Dark
}

/// Try OSC 11 query: `ESC ] 11 ; ? ST`
/// Modern terminals (kitty, iTerm2, WezTerm, foot, ghostty, Windows Terminal,
/// VSCode integrated) support this.
fn detect_via_osc11() -> Option<Theme> {
    // Only try on Unix terminals, skip if piped
    if !atty::is(atty::Stream::Stdout) {
        return None;
    }

    // Use a thread with timeout to avoid hanging the process.
    // Write OSC 11 query: ESC ] 11 ; ? BEL
    use std::io::{Read, Write};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    let mut stdout = std::io::stdout();
    let _ = write!(stdout, "\\x1b]11;?\\x07");
    let _ = stdout.flush();

    // Spawn a reader thread with a short lifespan
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 64];
        let mut total = 0;
        loop {
            match stdin.read(&mut buf[total..]) {
                Ok(0) => break,
                Ok(n) => {
                    total += n;
                    if buf[..total].contains(&0x07) || buf[..total].contains(&b'\x1b') {
                        break;
                    }
                    if total >= buf.len() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx.send((buf, total));
    });

    // Wait up to 100ms for the thread to get a response
    if let Ok((buf, total_read)) = rx.recv_timeout(Duration::from_millis(100)) {
        if total_read > 0 {
            let response = String::from_utf8_lossy(&buf[..total_read]);
            if let Some(rgb_part) = response.split(';').nth(1) {
                let rgb_str = rgb_part.trim_end_matches('\x07').trim();
                if let Some(rgb) = rgb_str.strip_prefix("rgb:") {
                    let parts: Vec<&str> = rgb.split('/').collect();
                    if parts.len() == 3 {
                        let r = parse_hex_channel(parts[0]);
                        let g = parse_hex_channel(parts[1]);
                        let b = parse_hex_channel(parts[2]);
                        if let (Some(r), Some(g), Some(b)) = (r, g, b) {
                            let lum = 0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64;
                            return Some(if lum > 150.0 { Theme::Light } else { Theme::Dark });
                        }
                    }
                }
            }
        }
    }

    None
}

/// Parse hex color channel (2 hex digits → 0-255) or (4 hex digits → 0-65535 → 0-255).
fn parse_hex_channel(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.len() == 4 {
        // 4-digit format: xxxx (0-65535)
        u16::from_str_radix(s, 16).ok().map(|v| v as f64 / 65535.0 * 255.0)
    } else if s.len() == 2 || s.len() == 1 {
        // 2-digit or 1-digit format
        u8::from_str_radix(s, 16).ok().map(|v| v as f64)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Color palettes
// ---------------------------------------------------------------------------

/// ANSI style strings for tree display, grouped by theme.
struct Palette {
    pub dir: &'static str,
    pub symlink: &'static str,
    pub source_code: &'static str,
    pub script: &'static str,
    pub image: &'static str,
    pub archive: &'static str,
    pub media: &'static str,
    pub config: &'static str,
    pub data: &'static str,
    pub binary: &'static str,
    pub markdown: &'static str,
    /// Used for annotations like size, line count, (N items)
    pub dim: &'static str,
    pub reset: &'static str,
}

/// Dark background palette (current ccat colors as 24-bit true color).
const DARK: &Palette = &Palette {
    dir: "\x1b[1;38;2;150;181;180m",   // bold #96b5b4 (base16 ocean cyan)
    symlink: "\x1b[1;38;2;235;203;139m", // bold #ebcb8b (base16 ocean yellow)
    source_code: "\x1b[38;2;163;190;140m", // #a3be8c (base16 ocean green)
    script: "\x1b[38;2;163;190;140m",    // #a3be8c
    image: "\x1b[38;2;180;142;173m",     // #b48ead (base16 ocean purple)
    archive: "\x1b[38;2;143;161;179m",   // #8fa1b3 (base16 ocean blue)
    media: "\x1b[38;2;208;135;112m",     // #d08770 (base16 ocean orange)
    config: "\x1b[38;2;163;190;140m",    // #a3be8c (same as code)
    data: "\x1b[38;2;163;190;140m",      // #a3be8c
    binary: "\x1b[1;38;2;191;97;106m",   // bold #bf616a (base16 ocean red)
    markdown: "\x1b[38;2;143;161;179m",  // #8fa1b3 (base16 ocean blue)
    dim: "\x1b[38;2;101;115;126m",       // #65737e (base16 ocean base0)
    reset: "\x1b[0m",
};

/// Light background palette — deeper, higher-contrast colors for white bg.
const LIGHT: &Palette = &Palette {
    dir: "\x1b[1;38;2;52;101;124m",     // bold darker teal
    symlink: "\x1b[1;38;2;157;120;40m",  // bold darker gold
    source_code: "\x1b[38;2;72;118;72m", // darker green
    script: "\x1b[38;2;72;118;72m",
    image: "\x1b[38;2;140;90;140m",      // darker purple
    archive: "\x1b[38;2;60;100;130m",    // darker blue
    media: "\x1b[38;2;170;100;60m",      // darker orange
    config: "\x1b[38;2;72;118;72m",      // same as code
    data: "\x1b[38;2;72;118;72m",
    binary: "\x1b[1;38;2;170;60;60m",    // bold darker red
    markdown: "\x1b[38;2;60;100;130m",   // darker blue
    dim: "\x1b[38;2;140;140;150m",       // mid-gray, readable on white
    reset: "\x1b[0m",
};

fn palette() -> &'static Palette {
    match active_theme() {
        Theme::Dark => DARK,
        Theme::Light => LIGHT,
    }
}

// ---------------------------------------------------------------------------
// Public API for tree styles
// ---------------------------------------------------------------------------

/// Returns style string for a file category, adapted to detected theme.
pub fn style_for(cat: crate::cat_tree::FileCategory) -> &'static str {
    use crate::cat_tree::FileCategory::*;
    let p = palette();
    match cat {
        Directory => p.dir,
        Symlink => p.symlink,
        SourceCode => p.source_code,
        Script => p.script,
        Image => p.image,
        Archive => p.archive,
        Media => p.media,
        Config => p.config,
        MarkdownDoc => p.markdown,
        Data | Plain => p.data,
        Binary => p.binary,
    }
}

pub fn dim_style() -> &'static str {
    palette().dim
}

pub fn reset_style() -> &'static str {
    palette().reset
}

// ---------------------------------------------------------------------------
// Public API for syntect highlighting
// ---------------------------------------------------------------------------

/// Returns the syntect theme name for source code highlighting.
pub fn syntect_theme_name() -> &'static str {
    match active_theme() {
        Theme::Dark => "base16-ocean.dark",
        Theme::Light => "base16-ocean.light",
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_default_is_dark() {
        // Without any env vars, should default to Dark
        assert_eq!(detect_from_env(), Theme::Dark);
    }

    #[test]
    fn test_env_var_dark() {
        temp_env::with_var("CCAT_COLOR_SCHEME", Some("dark"), || {
            assert_eq!(detect_from_env(), Theme::Dark);
        });
    }

    #[test]
    fn test_env_var_light() {
        temp_env::with_var("CCAT_COLOR_SCHEME", Some("light"), || {
            assert_eq!(detect_from_env(), Theme::Light);
        });
    }

    #[test]
    fn test_colorfgbg_light() {
        // COLORFGBG for light background: "0;15"
        temp_env::with_var("COLORFGBG", Some("0;15"), || {
            assert_eq!(detect_from_env(), Theme::Light);
        });
    }

    #[test]
    fn test_colorfgbg_dark() {
        temp_env::with_var("COLORFGBG", Some("15;0"), || {
            assert_eq!(detect_from_env(), Theme::Dark);
        });
    }

    #[test]
    fn test_iter_profile_light() {
        temp_env::with_var("ITERM_PROFILE", Some("Solarized Light"), || {
            assert_eq!(detect_from_env(), Theme::Light);
        });
    }

    #[test]
    fn test_konsole_profile_dark() {
        temp_env::with_var("KONSOLE_PROFILE_NAME", Some("Dark Pastels"), || {
            assert_eq!(detect_from_env(), Theme::Dark);
        });
    }

    #[test]
    fn test_palette_consistency() {
        // Both palettes should have non-empty reset
        assert!(!DARK.reset.is_empty());
        assert!(!LIGHT.reset.is_empty());
        // Dark dim should be a dark-ish color (lower luminance components)
        // Light dim should be a light-gray-ish color
        assert_ne!(DARK.dim, LIGHT.dim);
    }

    #[test]
    fn test_parse_hex_channel_2digit() {
        assert_eq!(parse_hex_channel("FF"), Some(255.0));
        assert_eq!(parse_hex_channel("00"), Some(0.0));
        assert_eq!(parse_hex_channel("80"), Some(128.0));
    }

    #[test]
    fn test_parse_hex_channel_4digit() {
        let val = parse_hex_channel("FFFF").unwrap();
        assert!((val - 255.0).abs() < 1.0);
        let val = parse_hex_channel("8000").unwrap();
        assert!((val - 128.0).abs() < 1.0);
    }

    #[test]
    fn test_parse_hex_channel_invalid() {
        assert!(parse_hex_channel("").is_none());
        assert!(parse_hex_channel("GGG").is_none());
    }

    #[test]
    fn test_force_theme() {
        force_theme(Some(Theme::Light));
        assert_eq!(active_theme(), Theme::Light);
        assert_eq!(syntect_theme_name(), "base16-ocean.light");
        force_theme(Some(Theme::Dark));
        assert_eq!(active_theme(), Theme::Dark);
        assert_eq!(syntect_theme_name(), "base16-ocean.dark");
    }
}
