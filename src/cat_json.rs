use std::io::Write;

/// Pretty-print and syntax-highlight JSON.
pub fn cat_json(data: &[u8]) {
    let s = String::from_utf8_lossy(data);
    match serde_json::from_str::<serde_json::Value>(&s) {
        Ok(value) => {
            let pretty = serde_json::to_string_pretty(&value).unwrap_or_else(|_| s.to_string());
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            for line in pretty.lines() {
                let colored = colorize_json_line(line);
                let _ = writeln!(handle, "{}", colored);
            }
        }
        Err(_) => {
            print!("{s}");
        }
    }
}

fn colorize_json_line(line: &str) -> String {
    let mut out = String::new();
    let trimmed = line.trim();
    let indent = &line[..line.len() - trimmed.len()];

    out.push_str(indent);

    // Detect key-value lines: "key": value
    if let Some(rest) = trimmed.strip_prefix('"') {
        // Find the closing quote and colon
        if let Some(end_quote) = rest.find('"') {
            let key = &rest[..end_quote];
            let after = &rest[end_quote + 1..];
            // Check for colon
            if after.trim_start().starts_with(':') {
                out.push_str(&format!("\x1b[33m\"{}\"\x1b[0m", key)); // yellow key

                // Parse value part
                let val_part = after.trim_start().trim_start_matches(':').trim();
                out.push_str(": ");
                out.push_str(&colorize_value(val_part));
                return out;
            }
        }
    }

    // Not a key-value line — try to colorize the whole thing
    if trimmed.starts_with('{') || trimmed.starts_with('}') || trimmed.starts_with('[') || trimmed.starts_with(']') {
        out.push_str(&format!("\x1b[2m{}\x1b[0m", trimmed));
        return out;
    }

    out.push_str(&colorize_value(trimmed));
    out
}

fn colorize_value(val: &str) -> String {
    let val = val.trim_end_matches(',');
    let trailing = if val.len() < val.trim_end_matches(',').len() { "" } else { "" };

    // String
    if val.starts_with('"') {
        format!("\x1b[32m{}\x1b[0m{}", val, trailing)
    }
    // Number
    else if val.starts_with('-') || val.starts_with(|c: char| c.is_ascii_digit()) {
        format!("\x1b[95m{}\x1b[0m{}", val, trailing)
    }
    // Boolean
    else if val == "true" || val == "false" {
        format!("\x1b[36m{}\x1b[0m{}", val, trailing)
    }
    // Null
    else if val == "null" {
        format!("\x1b[2m{}\x1b[0m{}", val, trailing)
    }
    // Array/object (inline)
    else if val.starts_with('[') || val.starts_with('{') {
        format!("\x1b[2m{}\x1b[0m{}", val, trailing)
    }
    else {
        format!("{}{}", val, trailing)
    }
}
