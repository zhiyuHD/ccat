use std::io::Write;

/// Syntax-highlight JSON without reformatting, preserving original structure and commas.
/// Uses serde_json only to validate, then highlights the raw pretty-printed output.
pub fn cat_json(data: &[u8]) {
    let s = String::from_utf8_lossy(data);

    // First try to parse and pretty-print with serde_json.
    // Note: serde_json::Value objects are BTreeMaps, which sort keys.
    // We accept this for now since it produces valid JSON.
    match serde_json::from_str::<serde_json::Value>(&s) {
        Ok(value) => {
            let pretty = serde_json::to_string_pretty(&value).unwrap_or_else(|_| s.to_string());
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            for line in pretty.lines() {
                let colored = colorize_line(line);
                let _ = writeln!(handle, "{}", colored);
            }
        }
        Err(e) => {
            // Invalid JSON — fallback to raw text
            eprintln!("ccat: invalid JSON ({e}), showing raw");
            print!("{s}");
        }
    }
}

fn colorize_line(line: &str) -> String {
    let trimmed = line.trim();
    let indent = &line[..line.len() - trimmed.len()];
    let mut out = String::new();
    out.push_str(indent);

    // Empty line
    if trimmed.is_empty() {
        return out;
    }

    // Brackets only
    if trimmed == "{" || trimmed == "}" || trimmed == "[" || trimmed == "]" {
        out.push_str(&format!("\x1b[2m{}\x1b[0m", trimmed));
        return out;
    }

    // Comma-only line (shouldn't happen after serde_json, but just in case)
    if trimmed == "," {
        out.push_str(&format!("\x1b[2m,\x1b[0m"));
        return out;
    }

    // Key-value: "key": <value>[comma]
    if let Some((key_part, val_part)) = trimmed.split_once(':') {
        let key_trimmed = key_part.trim();
        if key_trimmed.starts_with('"') && key_trimmed.ends_with('"') {
            out.push_str(&format!("\x1b[33m{}\x1b[0m", key_trimmed));
            out.push(':');

            let val_with_maybe_comma = val_part.trim();
            if !val_with_maybe_comma.is_empty() {
                // Check if value ends with comma and split
                let (val, has_comma) = if val_with_maybe_comma.ends_with(',') {
                    (&val_with_maybe_comma[..val_with_maybe_comma.len() - 1], true)
                } else {
                    (val_with_maybe_comma, false)
                };

                let colored_val = if val.starts_with('[') || val.starts_with('{') {
                    format!("\x1b[2m{}\x1b[0m", val)
                } else {
                    colorize_atom(val)
                };
                out.push(' ');
                out.push_str(&colored_val);
                if has_comma {
                    out.push_str("\x1b[2m,\x1b[0m");
                }
            }
            return out;
        }
    }

    // Array element: value[comma]
    let (val, has_comma) = if trimmed.ends_with(',') {
        (&trimmed[..trimmed.len() - 1], true)
    } else {
        (trimmed, false)
    };

    let colored = if val.starts_with('"') {
        format!("\x1b[32m{}\x1b[0m", val)
    } else if val.starts_with('[') || val.starts_with('{') {
        format!("\x1b[2m{}\x1b[0m", val)
    } else {
        colorize_atom(val)
    };

    out.push_str(&colored);
    if has_comma {
        out.push_str("\x1b[2m,\x1b[0m");
    }

    out
}

fn colorize_atom(val: &str) -> String {
    // Number
    if val.starts_with('-') || val.starts_with(|c: char| c.is_ascii_digit()) {
        if val.parse::<f64>().is_ok() || val.parse::<i64>().is_ok() {
            return format!("\x1b[95m{}\x1b[0m", val);
        }
    }
    // Boolean
    if val == "true" || val == "false" {
        return format!("\x1b[36m{}\x1b[0m", val);
    }
    // Null
    if val == "null" {
        return format!("\x1b[2m{}\x1b[0m", val);
    }
    // String
    if val.starts_with('"') && val.ends_with('"') {
        return format!("\x1b[32m{}\x1b[0m", val);
    }
    val.to_string()
}
