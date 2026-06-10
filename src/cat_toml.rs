use std::io::Write;

/// Syntax-highlight TOML output.
pub fn cat_toml(data: &[u8]) {
    let s = String::from_utf8_lossy(data);
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    for line in s.lines() {
        let colored = highlight_toml_line(line);
        let _ = writeln!(handle, "{}", colored);
    }
}

pub fn highlight_toml_line(line: &str) -> String {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];

    // Comment
    if trimmed.starts_with('#') {
        return format!("{indent}\x1b[2m{trimmed}\x1b[0m");
    }

    // Table header [section] or [[array]]
    if trimmed.starts_with('[') {
        if trimmed.contains('[') && trimmed.contains(']') {
            return format!("{indent}\x1b[1;36m{trimmed}\x1b[0m");
        }
        return format!("{indent}\x1b[36m{trimmed}\x1b[0m");
    }

    // Key-value
    if let Some(eq_pos) = trimmed.find('=') {
        let key = trimmed[..eq_pos].trim();
        let val = trimmed[eq_pos + 1..].trim();

        let key_colored = format!("\x1b[33m{key}\x1b[0m");

        // Inline table
        if val.starts_with('{') || val.starts_with('[') {
            return format!("{indent}{key_colored} = \x1b[2m{val}\x1b[0m");
        }

        let val_colored = colorize_toml_value(val);
        return format!("{indent}{key_colored} = {val_colored}");
    }

    // Plain value or empty
    line.to_string()
}

fn colorize_toml_value(val: &str) -> String {
    // String (quoted)
    if val.starts_with('"') && val.ends_with('"') {
        return format!("\x1b[32m{val}\x1b[0m");
    }
    if val.starts_with('\'') && val.ends_with('\'') {
        return format!("\x1b[32m{val}\x1b[0m");
    }
    // Number
    if val.starts_with('-') || val.starts_with(|c: char| c.is_ascii_digit()) {
        if val.parse::<i64>().is_ok() || val.parse::<f64>().is_ok() {
            return format!("\x1b[95m{val}\x1b[0m");
        }
    }
    // Boolean
    if val == "true" || val == "false" {
        return format!("\x1b[36m{val}\x1b[0m");
    }
    // Date/Time — TOML has offset-datetime, local-datetime, local-date, local-time
    if val.contains('-') || val.contains(':') || val.contains('T') {
        return format!("\x1b[94m{val}\x1b[0m");
    }
    val.to_string()
}
