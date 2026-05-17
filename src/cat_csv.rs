use std::io::Write;

/// Format CSV/TSV data as a table with aligned columns and colors.
pub fn cat_csv(data: &[u8]) {
    let s = String::from_utf8_lossy(data);
    let delimiter = if s.contains('\t') { b'\t' } else { b',' };

    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut widths: Vec<usize> = Vec::new();

    for line in s.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut cols = Vec::new();
        // Simple CSV parsing (no quoted fields for simplicity)
        for cell in line.split(delimiter as char) {
            let trimmed = cell.trim().trim_matches('"').to_string();
            cols.push(trimmed);
        }
        // Update column widths
        while widths.len() < cols.len() {
            widths.push(0);
        }
        for (i, col) in cols.iter().enumerate() {
            widths[i] = widths[i].max(col.len());
        }
        rows.push(cols);
    }

    if rows.is_empty() {
        return;
    }

    // Pad all rows to equal width
    for row in &mut rows {
        while row.len() < widths.len() {
            row.push(String::new());
        }
    }

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();

    // Header row (first row) in bold
    if let Some(header) = rows.first() {
        for (i, col) in header.iter().enumerate() {
            let _ = write!(handle, "\x1b[1m{:width$}\x1b[0m", col, width = widths[i]);
            if i + 1 < widths.len() {
                let _ = write!(handle, "  \x1b[2m│\x1b[0m  ");
            }
        }
        let _ = writeln!(handle);
    }

    // Separator
    for (i, w) in widths.iter().enumerate() {
        let _ = write!(handle, "\x1b[2m{}\x1b[0m", "─".repeat(*w));
        if i + 1 < widths.len() {
            let _ = write!(handle, "\x1b[2m──┼──\x1b[0m");
        }
    }
    let _ = writeln!(handle);

    // Data rows
    for row in rows.iter().skip(1) {
        for (i, col) in row.iter().enumerate() {
            // Numbers right-align, strings left-align
            if col.parse::<f64>().is_ok() || col == "true" || col == "false" || col == "null" {
                let _ = write!(
                    handle,
                    "\x1b[95m{:>width$}\x1b[0m",
                    col,
                    width = widths[i]
                );
            } else {
                let _ = write!(
                    handle,
                    "{:width$}",
                    col,
                    width = widths[i]
                );
            }
            if i + 1 < widths.len() {
                let _ = write!(handle, "  \x1b[2m│\x1b[0m  ");
            }
        }
        let _ = writeln!(handle);
    }
}
