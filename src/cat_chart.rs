use std::io::Write;
use crate::pager;

/// Configuration for chart rendering
#[derive(Debug, Clone, PartialEq)]
pub enum ChartType {
    /// Horizontal bar chart (good for categories, up to ~30 rows)
    Bar,
    /// Line chart using Unicode braille for smooth curves (good for time series)
    Line,
}

/// A parsed dataset ready for charting
#[derive(Debug)]
pub struct ChartData {
    /// Column headers / series names
    pub headers: Vec<String>,
    /// Data rows. For bar charts, each row is one series.
    /// For line charts, each row is one point, columns are Y values.
    pub rows: Vec<Vec<f64>>,
    /// Row labels (x-axis / category names)
    pub labels: Vec<String>,
}

/// Try to parse data as a chart dataset.
/// Returns Some(ChartData) if at least one numeric column is found.
pub fn parse_chart_data(data: &[u8], x_col: Option<&str>, y_col: Option<&str>) -> Option<ChartData> {
    let s = String::from_utf8_lossy(data);

    // Try CSV first
    if let Some(chart) = parse_csv_chart(&s, x_col, y_col) {
        return Some(chart);
    }

    // Try JSON array of objects
    if let Some(chart) = parse_json_chart(&s, x_col, y_col) {
        return Some(chart);
    }

    // Try raw numbers (whitespace-separated)
    if let Some(chart) = parse_number_list(&s) {
        return Some(chart);
    }

    None
}

fn parse_csv_chart(s: &str, x_col: Option<&str>, y_col: Option<&str>) -> Option<ChartData> {
    let delimiter = if s.contains('\t') && !s.contains(',') {
        b'\t'
    } else {
        b','
    };

    let raw_lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    if raw_lines.len() < 2 {
        return None;
    }

    let headers: Vec<String> = raw_lines[0]
        .split(delimiter as char)
        .map(|c| c.trim().trim_matches('"').to_string())
        .collect();

    if headers.len() < 2 {
        return None;
    }

    let mut rows: Vec<Vec<f64>> = Vec::new();
    let mut labels: Vec<String> = Vec::new();

    // Find x and y column indices
    let x_idx = x_col.and_then(|x| headers.iter().position(|h| h.eq_ignore_ascii_case(x)));
    let y_idx = y_col.and_then(|y| headers.iter().position(|h| h.eq_ignore_ascii_case(y)));

    for line in raw_lines.iter().skip(1) {
        let cols: Vec<&str> = line.split(delimiter as char).collect();
        if cols.len() < 2 {
            continue;
        }

        // Get x-label (either specified column or first column)
        let label = if let Some(idx) = x_idx {
            cols.get(idx)
                .map(|c| c.trim().trim_matches('"').to_string())
                .unwrap_or_default()
        } else {
            cols[0].trim().trim_matches('"').to_string()
        };

        // Get y-values
        let y_vals: Vec<f64> = if let Some(idx) = y_idx {
            // Single specific column
            cols.get(idx)
                .and_then(|c| c.trim().parse().ok())
                .map(|v| vec![v])
                .unwrap_or_default()
        } else {
            // All numeric columns (except the label column)
            let label_idx = x_idx.or(Some(0)).unwrap_or(0);
            cols.iter()
                .enumerate()
                .filter(|(i, c)| *i != label_idx && c.trim().parse::<f64>().is_ok())
                .map(|(_, c)| c.trim().parse::<f64>().unwrap())
                .collect()
        };

        if !y_vals.is_empty() {
            labels.push(label);
            rows.push(y_vals);
        }
    }

    if rows.is_empty() {
        return None;
    }

    Some(ChartData {
        headers: if let Some(idx) = y_idx {
            vec![headers[idx].clone()]
        } else if y_col.is_some() {
            // y_col specified but not found — fallback to first numeric column
            vec!["value".to_string()]
        } else {
            // Collect all numeric column headers
            let label_idx = x_idx.or(Some(0)).unwrap_or(0);
            headers
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != label_idx)
                .map(|(_, h)| h.clone())
                .collect()
        },
        rows,
        labels,
    })
}

fn parse_json_chart(s: &str, x_col: Option<&str>, y_col: Option<&str>) -> Option<ChartData> {
    // Try to parse as JSON array of objects
    let val: serde_json::Value = serde_json::from_str(s).ok()?;

    let arr = match &val {
        serde_json::Value::Array(a) => a,
        serde_json::Value::Object(o) => {
            // Maybe {"data": [...]} or similar wrapper
            o.values().find_map(|v| v.as_array())?
        }
        _ => return None,
    };

    if arr.is_empty() {
        return None;
    }

    // Collect all keys that appear in most objects
    let mut key_count: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for obj in arr.iter().filter_map(|v| v.as_object()) {
        for key in obj.keys() {
            *key_count.entry(key.clone()).or_insert(0) += 1;
        }
    }

    // Filter to keys present in at least 40% of objects, preferring numeric ones
    let threshold = (arr.len() * 4 + 5) / 10;
    let numeric_keys: Vec<&str> = key_count
        .iter()
        .filter(|&(_, &count)| count >= threshold)
        .filter(|(key, _)| {
            arr.iter()
                .filter_map(|v| v.as_object())
                .filter_map(|o| o.get(key.as_str()))
                .any(|v| v.is_number())
        })
        .map(|(k, _)| k.as_str())
        .collect();

    if numeric_keys.is_empty() {
        return None;
    }

    // Determine x-axis key
    let x_key: &str = x_col.unwrap_or_else(|| {
        // Try common label keys first
        for label_key in &["name", "label", "date", "time", "timestamp", "key", "x"] {
            if key_count.contains_key(*label_key) && !numeric_keys.iter().any(|k| k == label_key) {
                return label_key;
            }
        }
        // Use first non-numeric key if available
        let non_numeric = key_count
            .keys()
            .find(|k| !numeric_keys.contains(&k.as_str()));
        match non_numeric {
            Some(k) => k.as_str(),
            None => numeric_keys[0],
        }
    });

    // Determine y-axis keys
    let y_keys: Vec<&str> = if let Some(y) = y_col {
        let found = numeric_keys.iter().find(|k| k.eq_ignore_ascii_case(y));
        vec![match found {
            Some(k) => k,
            None => numeric_keys[0],
        }]
    } else {
        numeric_keys
            .iter()
            .filter(|k| !k.eq_ignore_ascii_case(x_key))
            .take(5)
            .copied()
            .collect()
    };

    let mut rows: Vec<Vec<f64>> = Vec::new();
    let mut labels: Vec<String> = Vec::new();

    for obj in arr.iter().filter_map(|v| v.as_object()) {
        let label = obj
            .get(x_key)
            .map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                _ => format!("{v}"),
            })
            .unwrap_or_default();

        let vals: Vec<f64> = y_keys
            .iter()
            .filter_map(|k| obj.get(*k))
            .filter_map(|v| match v {
                serde_json::Value::Number(n) => n.as_f64(),
                _ => None,
            })
            .collect();

        if !vals.is_empty() {
            labels.push(label);
            rows.push(vals);
        }
    }

    if rows.is_empty() {
        return None;
    }

    Some(ChartData {
        headers: y_keys.iter().map(|k| k.to_string()).collect(),
        rows,
        labels,
    })
}

fn parse_number_list(s: &str) -> Option<ChartData> {
    let nums: Vec<f64> = s
        .split_whitespace()
        .filter_map(|t| t.parse().ok())
        .collect();

    if nums.is_empty() {
        return None;
    }

    let rows: Vec<Vec<f64>> = nums.into_iter().map(|n| vec![n]).collect();
    let labels: Vec<String> = (1..=rows.len()).map(|i| i.to_string()).collect();

    Some(ChartData {
        headers: vec!["value".to_string()],
        rows,
        labels,
    })
}

/// Render a chart to stdout.
pub fn render_chart(data: &ChartData, chart_type: &ChartType, width: Option<usize>) {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();

    let term_width = width.unwrap_or_else(|| {
        let (_, w) = pager::terminal_size();
        w.max(40).min(160)
    });

    match chart_type {
        ChartType::Bar => render_bar_chart(&mut handle, data, term_width),
        ChartType::Line => render_line_chart(&mut handle, data, term_width),
    }
}

// ── Horizontal bar chart ────────────────────────────────────────

fn render_bar_chart(handle: &mut impl Write, data: &ChartData, term_width: usize) {
    let n_series = data.headers.len();
    let n_rows = data.rows.len();
    if n_rows == 0 {
        let _ = writeln!(handle, "{}no data{}", "\x1b[2m", "\x1b[0m");
        return;
    }

    // Find global max for scaling
    let global_max = data
        .rows
        .iter()
        .flat_map(|r| r.iter())
        .cloned()
        .fold(0.0_f64, f64::max);

    if global_max == 0.0 {
        let _ = writeln!(handle, "{}all values are zero{}", "\x1b[2m", "\x1b[0m");
        return;
    }

    // Calculate label width
    let label_width = data
        .labels
        .iter()
        .map(|l| l.len())
        .max()
        .unwrap_or(0)
        .min(30);
    let val_width = 8; // enough for " 123.45 "

    // Bar area width
    let bar_max = term_width.saturating_sub(label_width + val_width + 3);
    let bar_width = bar_max.max(10);

    // Colors for series (cycling through 8 distinguishable ANSI colors)
    const COLORS: &[&str] = &[
        "\x1b[36m", // cyan
        "\x1b[33m", // yellow
        "\x1b[35m", // magenta
        "\x1b[32m", // green
        "\x1b[34m", // blue
        "\x1b[31m", // red
        "\x1b[37m", // white
        "\x1b[38;5;208m", // orange
    ];
    const RESET: &str = "\x1b[0m";
    const DIM: &str = "\x1b[2m";

    // ── Title ──
    if n_series == 1 {
        let _ = writeln!(handle, "{}{}{}", DIM, &data.headers[0], RESET);
    } else {
        let _ = writeln!(handle, "{}series:{}", DIM, RESET);
        for (i, h) in data.headers.iter().enumerate() {
            let c = COLORS[i % COLORS.len()];
            let _ = writeln!(handle, "  {}■{} {}", c, RESET, h);
        }
    }
    let _ = writeln!(handle);

    // ── Bars ──
    for (row_idx, row) in data.rows.iter().enumerate() {
        let label = &data.labels[row_idx];
        let truncated: String = if label.len() > label_width {
            label.chars().take(label_width.saturating_sub(3)).collect::<String>() + "..."
        } else {
            format!("{:width$}", label, width = label_width)
        };

        let _ = write!(handle, "{} ", truncated);

        for (series_idx, value) in row.iter().enumerate() {
            let frac = value / global_max;
            let bar_len = (frac * bar_width as f64).round() as usize;
            let bar_len = bar_len.min(bar_width);
            let color = COLORS[series_idx % COLORS.len()];

            let bar: String = "█".repeat(bar_len);
            let _ = write!(handle, "{}{}{}", color, bar, RESET);
        }

        let remaining = bar_width
            .saturating_sub(
                row.iter()
                    .enumerate()
                    .map(|(_, v)| {
                        let f = v / global_max;
                        (f * bar_width as f64).round() as usize
                    })
                    .sum::<usize>(),
            )
            .min(bar_width);

        if remaining > 0 {
            let _ = write!(handle, "{}", " ".repeat(remaining));
        }

        // Value label
        let vals_str = if row.len() == 1 {
            format!("{:>8.1}", row[0])
        } else {
            format!(
                "{:>8.1}",
                row.iter().sum::<f64>()
            )
        };
        let _ = writeln!(handle, " {}", vals_str);
    }
}

// ── Line chart (braille-based) ──────────────────────────────────

/// Renders a line chart using Unicode braille characters for smooth curves.
/// Braille dots: ⡀⡄⡆⡇⣀⣄⣆⣇⣤⣦⣧⣷⣿ etc.
/// Each braille cell gives 2×4 = 8 pixel resolution.
fn render_line_chart(handle: &mut impl Write, data: &ChartData, term_width: usize) {
    let n_series = data.headers.len();
    let n_points = data.rows.len();

    if n_points < 2 {
        // Not enough points for a line chart, fall back to bar
        return render_bar_chart(handle, data, term_width);
    }

    // Find global min/max for Y-axis
    let mut y_min = f64::MAX;
    let mut y_max = f64::MIN;
    for row in &data.rows {
        for &v in row {
            if v < y_min { y_min = v; }
            if v > y_max { y_max = v; }
        }
    }

    if y_min == y_max {
        let _ = writeln!(handle, "{}⚠ all values = {:.1}{}", "\x1b[33m", y_min, "\x1b[0m");
        return;
    }

    let y_label_width = 8;
    let chart_width = term_width.saturating_sub(y_label_width + 2).max(20);
    let chart_height = 16.min(n_points.max(4)); // lines of braille
    let pixel_height = chart_height * 4;

    const RESET: &str = "\x1b[0m";
    const DIM: &str = "\x1b[2m";

    const COLORS: &[&str] = &[
        "\x1b[36m", "\x1b[33m", "\x1b[35m",
        "\x1b[32m", "\x1b[34m", "\x1b[31m",
    ];

    // ── Title ──
    if n_series == 1 {
        let _ = writeln!(handle, "{}{}{}", DIM, &data.headers[0], RESET);
    } else {
        let _ = writeln!(handle, "{}series:{}", DIM, RESET);
        for (i, h) in data.headers.iter().enumerate() {
            let c = COLORS[i % COLORS.len()];
            let _ = writeln!(handle, "  {}─{} {}", c, RESET, h);
        }
    }
    let _ = writeln!(handle);

    // Braille cell columns: each column covers (n_points / chart_width) data points
    // For n_points <= chart_width, each point gets its own column
    let cols = n_points.min(chart_width);

    // For each series, compute y_pixel for each data point
    let mut series_pixels: Vec<Vec<usize>> = Vec::new();
    for series_idx in 0..n_series {
        let pixels: Vec<usize> = data.rows.iter().map(|row| {
            let v = row.get(series_idx).copied().unwrap_or(0.0);
            let normalized = (v - y_min) / (y_max - y_min);
            let y_pixel = ((1.0 - normalized) * (pixel_height - 1) as f64).round() as usize;
            y_pixel.min(pixel_height - 1)
        }).collect();
        series_pixels.push(pixels);
    }

    // Map data point index -> braille column
    let col_for_point = |i: usize| -> usize {
        if n_points <= 1 { return 0; }
        (i * (cols - 1)) / (n_points - 1)
    };

    // Build the braille grid
    // Each cell: byte where bit 0=dot1(top), 1=dot2, 2=dot3, 3=dot7(bottom)
    let mut grid: Vec<Vec<u8>> = vec![vec![0u8; cols]; chart_height];

    for series_idx in 0..n_series {
        let pixels = &series_pixels[series_idx];

        // Helper to set a pixel in the grid
        let set_pixel = |grid: &mut [Vec<u8>], pixel_y: usize, col: usize| {
            if pixel_y >= pixel_height || col >= cols { return; }
            let row = pixel_y / 4;
            let bit = match pixel_y % 4 {
                0 => 0x01, 1 => 0x02, 2 => 0x04, 3 => 0x08,
                _ => return,
            };
            grid[row][col] |= bit;
        };

        // Plot points and draw lines between them
        for i in 0..n_points {
            let col_i = col_for_point(i);
            set_pixel(&mut grid, pixels[i], col_i);

            if i + 1 < n_points {
                let col_j = col_for_point(i + 1);
                let y1 = pixels[i] as f64;
                let y2 = pixels[i + 1] as f64;

                // Interpolate along the line from (col_i, y1) to (col_j, y2)
                if col_j > col_i {
                    let cols_between = col_j - col_i;
                    for c in 0..=cols_between {
                        let t = c as f64 / cols_between as f64;
                        let mid_y = (y1 + (y2 - y1) * t).round() as usize;
                        let col_mid = col_i + c;
                        set_pixel(&mut grid, mid_y.min(pixel_height - 1), col_mid);
                    }
                } else {
                    // Same column — fill all pixel rows between y1 and y2
                    let y_start = y1.min(y2) as usize;
                    let y_end = y1.max(y2) as usize;
                    for y in y_start..=y_end {
                        set_pixel(&mut grid, y.min(pixel_height - 1), col_i);
                    }
                }
            }
        }
    }

    // ── Render ──
    let line_color = if n_series == 1 {
        COLORS[0]
    } else {
        "\x1b[38;5;117m"
    };

    for row in (0..chart_height).rev() {
        // Y-axis label
        let label_y = if row == chart_height - 1 {
            y_max
        } else if row == 0 {
            y_min
        } else {
            y_min + (y_max - y_min) * (1.0 - row as f64 / (chart_height - 1) as f64)
        };
        let _ = write!(handle, "{}{:>8.1}{} ", DIM, label_y, RESET);

        for col in 0..cols {
            let bits = grid[row][col];
            if bits == 0 {
                let _ = write!(handle, " ");
            } else {
                let ch = braille_from_bits(bits);
                let _ = write!(handle, "{}{}{}", line_color, ch, RESET);
            }
        }
        let _ = writeln!(handle);
    }

    // ── X-axis ──
    let _ = write!(handle, "{}", " ".repeat(y_label_width + 1));
    let _ = write!(handle, "{}", DIM);
    for _ in 0..cols {
        let _ = write!(handle, "─");
    }
    let _ = writeln!(handle, "{}", RESET);

    // ── X-axis labels ──
    let _ = write!(handle, "{}", " ".repeat(y_label_width + 1));
    let first_label = truncate_str(&data.labels[0], 8);
    let mid_label = truncate_str(&data.labels[n_points / 2], 8);
    let last_label = truncate_str(&data.labels[n_points - 1], 8);
    let _ = writeln!(handle, "{}{}  {}  {}{}", DIM, first_label, mid_label, last_label, RESET);

    let _ = writeln!(handle, "{}data points: {}, range [{:.1}, {:.1}]{}",
        DIM, n_points, y_min, y_max, RESET
    );
}

fn braille_from_bits(bits: u8) -> char {
    // Standard braille dot mapping for 4-bit patterns
    // We use a compact representation:
    // bits 0→dot1(0x01), 1→dot2(0x02), 2→dot3(0x04), 3→dot7(0x08)
    // Full braille: U+2800 + dot_pattern
    // Where dot7 is at position 7 (bit 3 in our 4-bit = bit 7 in Unicode)
    let unicode_bitmap = match bits {
        0 => 0x2800, // blank
        // Common 1-dot patterns
        0x01 => 0x2801, // ⠁
        0x02 => 0x2802, // ⠂
        0x04 => 0x2804, // ⠄
        0x08 => 0x2808, // ⠈
        // 2-dot patterns
        0x03 => 0x2803, // ⠃
        0x05 => 0x2805, // ⠅
        0x06 => 0x2806, // ⠆
        0x09 => 0x2809, // ⠉
        0x0A => 0x280A, // ⠊
        0x0C => 0x280C, // ⠌
        // 3-dot patterns
        0x07 => 0x2807, // ⠇
        0x0B => 0x280B, // ⠋
        0x0D => 0x280D, // ⠍
        0x0E => 0x280E, // ⠎
        // 4-dot pattern
        0x0F => 0x280F, // ⠏
        _ => 0x2800 + bits as u32,
    };
    char::from_u32(unicode_bitmap).unwrap_or(' ')
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        s.chars().take(max_len.saturating_sub(3)).collect::<String>() + "..."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_csv_basic() {
        let csv = "name,age,score\nAlice,30,95\nBob,25,87\nCharlie,35,92\n";
        let data = parse_chart_data(csv.as_bytes(), None, None).unwrap();
        assert_eq!(data.labels, vec!["Alice", "Bob", "Charlie"]);
        assert_eq!(data.headers, vec!["age", "score"]);
        assert_eq!(data.rows.len(), 3);
        assert_eq!(data.rows[0], vec![30.0, 95.0]);
    }

    #[test]
    fn test_parse_csv_with_x_col() {
        let csv = "name,age,score\nAlice,30,95\nBob,25,87\n";
        let data = parse_chart_data(csv.as_bytes(), Some("name"), Some("score")).unwrap();
        assert_eq!(data.labels, vec!["Alice", "Bob"]);
        assert_eq!(data.headers, vec!["score"]);
        assert_eq!(data.rows[0], vec![95.0]);
    }

    #[test]
    fn test_parse_csv_single_column() {
        let csv = "value\n10\n20\n30\n40\n";
        let data = parse_chart_data(csv.as_bytes(), None, None);
        assert!(data.is_some());
        let data = data.unwrap();
        assert_eq!(data.rows.len(), 4);
        assert_eq!(data.rows[0], vec![10.0]);
    }

    #[test]
    fn test_parse_json_array() {
        let json = r#"[
            {"name": "Jan", "sales": 100, "cost": 50},
            {"name": "Feb", "sales": 150, "cost": 70},
            {"name": "Mar", "sales": 130, "cost": 60}
        ]"#;
        let data = parse_chart_data(json.as_bytes(), None, None).unwrap();
        assert_eq!(data.labels, vec!["Jan", "Feb", "Mar"]);
        assert!(data.headers.contains(&"sales".to_string()));
        assert_eq!(data.rows.len(), 3);
    }

    #[test]
    fn test_parse_number_list() {
        let nums = "10 20 30 40 50";
        let data = parse_chart_data(nums.as_bytes(), None, None).unwrap();
        assert_eq!(data.rows.len(), 5);
        assert_eq!(data.rows[0], vec![10.0]);
        assert_eq!(data.rows[4], vec![50.0]);
    }

    #[test]
    fn test_parse_empty() {
        assert!(parse_chart_data(b"", None, None).is_none());
        assert!(parse_chart_data(b"no numbers here", None, None).is_none());
    }

    #[test]
    fn test_parse_tsv() {
        let tsv = "name\tage\tscore\nAlice\t30\t95\nBob\t25\t87\n";
        let data = parse_chart_data(tsv.as_bytes(), None, None).unwrap();
        assert_eq!(data.labels, vec!["Alice", "Bob"]);
        assert_eq!(data.rows[0], vec![30.0, 95.0]);
    }

    #[test]
    fn test_bar_chart_render_no_crash() {
        let data = ChartData {
            headers: vec!["score".to_string()],
            rows: vec![vec![95.0], vec![87.0], vec![92.0]],
            labels: vec!["Alice".to_string(), "Bob".to_string(), "Charlie".to_string()],
        };
        let mut buf = Vec::new();
        // Render into buffer, width 60
        render_bar_chart(&mut buf, &data, 60);
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("Alice"));
        assert!(output.contains("95.0"));
        assert!(output.contains("█"));
    }

    #[test]
    fn test_line_chart_render_no_crash() {
        let data = ChartData {
            headers: vec!["value".to_string()],
            rows: (1..=20).map(|i| vec![i as f64]).collect(),
            labels: (1..=20).map(|i| i.to_string()).collect(),
        };
        let mut buf = Vec::new();
        let term_width = 60usize;
        // We can't easily capture render_line_chart output because it writes to handle,
        // but we can verify it doesn't panic
        render_line_chart(&mut buf, &data, term_width);
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("20"));
        assert!(output.len() > 100);
    }

    #[test]
    fn test_multi_series_csv() {
        let csv = "year,revenue,profit\n2020,100,20\n2021,150,30\n2022,200,40\n";
        let data = parse_chart_data(csv.as_bytes(), None, None).unwrap();
        assert_eq!(data.headers, vec!["revenue", "profit"]);
        assert_eq!(data.rows[0], vec![100.0, 20.0]);
        assert_eq!(data.rows[2], vec![200.0, 40.0]);
    }

    #[test]
    fn test_braille_from_bits() {
        assert_eq!(braille_from_bits(0), '\u{2800}');
        assert_eq!(braille_from_bits(0x01), '\u{2801}');
        assert_eq!(braille_from_bits(0x0F), '\u{280F}');
    }
}
