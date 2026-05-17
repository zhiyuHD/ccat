use std::io::Write;

/// Syntax-highlight YAML output.
pub fn cat_yaml(data: &[u8]) {
    let s = String::from_utf8_lossy(data);
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    for line in s.lines() {
        let colored = highlight_yaml_line(line);
        let _ = writeln!(handle, "{}", colored);
    }
    if !s.ends_with('\n') {
        // ensure trailing newline
    }
}

fn highlight_yaml_line(line: &str) -> String {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];

    // Comment
    if trimmed.starts_with('#') {
        return format!("{indent}\x1b[2m{trimmed}\x1b[0m");
    }

    // Key-value: key: value
    if let Some(col_pos) = trimmed.find(':') {
        let after_colon = &trimmed[col_pos + 1..];
        if after_colon.starts_with(' ') || after_colon.is_empty() {
            let key = &trimmed[..col_pos];
            let val_part = after_colon.trim();

            // List item "- key: value"
            let prefix = if key.starts_with("- ") {
                let dash_end = key.find(' ').unwrap_or(1);
                let rest = key[dash_end..].trim();
                format!("\x1b[2m- \x1b[0m\x1b[33m{}\x1b[0m", rest)
            } else {
                format!("\x1b[33m{}\x1b[0m", key)
            };

            let val_colored = if val_part.is_empty() {
                String::new()
            } else {
                format!(": {}", colorize_yaml_value(val_part))
            };

            return format!("{indent}{prefix}{val_colored}");
        }
    }

    // List item "- value"
    if let Some(rest) = trimmed.strip_prefix("- ") {
        return format!("{indent}\x1b[2m- \x1b[0m{}", colorize_yaml_value(rest));
    }

    // Plain value (continuation)
    if !trimmed.is_empty() {
        return format!("{indent}{}", colorize_yaml_value(trimmed));
    }

    line.to_string()
}

fn colorize_yaml_value(val: &str) -> String {
    // String (quoted)
    if (val.starts_with('"') && val.ends_with('"')) || (val.starts_with('\'') && val.ends_with('\'')) {
        return format!("\x1b[32m{}\x1b[0m", val);
    }
    // Number
    if val.starts_with('-') || val.starts_with(|c: char| c.is_ascii_digit()) {
        if val.parse::<f64>().is_ok() {
            return format!("\x1b[95m{}\x1b[0m", val);
        }
    }
    // Boolean
    if val == "true" || val == "false" || val == "yes" || val == "no" || val == "on" || val == "off" {
        return format!("\x1b[36m{}\x1b[0m", val);
    }
    // Null
    if val == "null" || val == "~" {
        return format!("\x1b[2m{}\x1b[0m", val);
    }

    val.to_string()
}
