/// Utility functions for HTTP toolkit.

/// Convert a status code to a colored string for terminal output.
pub fn status_color(code: u16) -> &'static str {
    if code < 300 {
        "\x1b[32m" // Green
    } else if code < 400 {
        "\x1b[33m" // Yellow
    } else if code < 500 {
        "\x1b[34m" // Blue
    } else {
        "\x1b[31m" // Red
    }
}

/// Reset color
pub const fn color_reset() -> &'static str {
    "\x1b[0m"
}

/// Format bytes into human-readable size.
pub fn human_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_human_size() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(500), "500 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
    }

    #[test]
    fn test_status_color() {
        assert_eq!(status_color(200), "\x1b[32m");
        assert_eq!(status_color(301), "\x1b[33m");
        assert_eq!(status_color(404), "\x1b[34m");
        assert_eq!(status_color(500), "\x1b[31m");
    }
}
