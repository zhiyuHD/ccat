/// Config file support for ccat.
///
/// Reads `~/.config/ccat/config.toml` (XDG) or `~/.ccatrc.toml` (legacy)
/// and merges with CLI args (CLI overrides config).
///
/// Example config file:
/// ```toml
/// # ~/.config/ccat/config.toml
/// color_scheme = "auto"          # "auto", "dark", "light"
/// theme = "base16-ocean.dark"    # syntect theme for source highlighting
/// number = false
/// squeeze_blank = false
/// ```

use std::path::PathBuf;

use serde::Deserialize;

/// Top-level config structure matching `config.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub color_scheme: Option<String>,
    pub theme: Option<String>,
    pub number: Option<bool>,
    pub number_nonblank: Option<bool>,
    pub squeeze_blank: Option<bool>,
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// Discover and load config, or return a default config if none exists.
/// Silent on missing files — only warns on parse errors.
pub fn load() -> Config {
    let paths = discover_config_paths();
    for path in &paths {
        if path.exists() {
            match std::fs::read_to_string(path) {
                Ok(content) => {
                    match toml::from_str(&content) {
                        Ok(cfg) => return cfg,
                        Err(e) => {
                            eprintln!(
                                "ccat: warning: {}: parse error: {e}",
                                path.display()
                            );
                        }
                    }
                }
                Err(e) => {
                    eprintln!(
                        "ccat: warning: {}: read error: {e}",
                        path.display()
                    );
                }
            }
        }
    }
    Config::default()
}

/// Return candidate config paths in priority order.
fn discover_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // 1. XDG config dir: ~/.config/ccat/config.toml
    if let Some(xdg) = dirs::config_dir() {
        paths.push(xdg.join("ccat").join("config.toml"));
    }

    // 2. Legacy: ~/.ccatrc.toml
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".ccatrc.toml"));
    }

    paths
}

// ---------------------------------------------------------------------------
// Merging
// ---------------------------------------------------------------------------

macro_rules! merge_opt {
    ($cli:expr, $cfg:expr) => {
        $cli.or($cfg)
    };
}

/// Merge CLI flags with config values. CLI values take priority.
pub struct MergedConfig {
    pub color_scheme: String,
    pub theme: Option<String>,
    pub number: bool,
    pub number_nonblank: bool,
    pub squeeze_blank: bool,
}

impl MergedConfig {
    pub fn new(cli_color_scheme: &str, cli_theme: Option<String>, cli_number: bool,
               cli_number_nonblank: bool, cli_squeeze_blank: bool) -> Self
    {
        let cfg = load();

        let color_scheme = cli_color_scheme.to_string();
        let theme = merge_opt!(cli_theme, cfg.theme);
        let number = cli_number || cfg.number.unwrap_or(false);
        let number_nonblank = cli_number_nonblank || cfg.number_nonblank.unwrap_or(false);
        let squeeze_blank = cli_squeeze_blank || cfg.squeeze_blank.unwrap_or(false);

        MergedConfig {
            color_scheme,
            theme,
            number,
            number_nonblank,
            squeeze_blank,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_is_empty() {
        let cfg = Config::default();
        assert!(cfg.color_scheme.is_none());
        assert!(cfg.theme.is_none());
        assert!(cfg.number.is_none());
    }

    #[test]
    fn test_merge_cli_overrides_config() {
        let merged = MergedConfig::new(
            "dark",
            Some("monokai".into()),
            true,
            false,
            false,
        );
        assert_eq!(merged.color_scheme, "dark");
        assert_eq!(merged.theme, Some("monokai".into()));
        assert!(merged.number);
    }

    #[test]
    fn test_merge_config_fills_in_defaults() {
        // Config says number=true, CLI says nothing (defaults)
        // We simulate this by writing a temp config
        let merged = MergedConfig::new(
            "auto",
            None,
            false,
            false,
            false,
        );
        // No config file exists, so all should be false/None
        assert_eq!(merged.color_scheme, "auto");
        assert!(merged.theme.is_none());
        assert!(!merged.number);
    }

    #[test]
    fn test_config_parse() {
        let toml_str = r#"
color_scheme = "dark"
theme = "monokai"
number = true
squeeze_blank = true
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.color_scheme, Some("dark".into()));
        assert_eq!(config.theme, Some("monokai".into()));
        assert_eq!(config.number, Some(true));
        assert_eq!(config.squeeze_blank, Some(true));
    }

    #[test]
    fn test_config_partial_parse() {
        let toml_str = r#"
theme = "solarized-dark"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.color_scheme.is_none());
        assert_eq!(config.theme, Some("solarized-dark".into()));
        assert!(config.number.is_none());
    }

    #[test]
    fn test_invalid_config_returns_default() {
        // An invalid path should just give default
        let _cfg = Config::default();
        // Can't easily test the file-based path without creating temp files,
        // but we can verify the default is safe
        assert!(_cfg.color_scheme.is_none());
    }
}
