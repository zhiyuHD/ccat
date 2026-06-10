use serde_json::Value;
use std::io::Write;
use std::path::Path;

// ── tree-drawing characters ──
const T_BRANCH: &str = "├─ ";
const T_LAST: &str = "└─ ";
const T_VLINE: &str = "│  ";
const T_SPACE: &str = "   ";
const T_ROOT: &str = "◉ ";

// ── schema node types ──

#[derive(Debug, Clone)]
enum SchemaNode {
    Object {
        children: Vec<(String, SchemaNode)>,
        total_keys: usize,
        depth: usize,
    },
    Array {
        element: Box<SchemaNode>,
        count: usize,
        min_len: usize,
        max_len: usize,
    },
    String {
        nulls: usize,
        total: usize,
        min_len: usize,
        max_len: usize,
        distinct: Option<usize>,
        is_date: bool,
        is_url: bool,
        is_email: bool,
    },
    Integer {
        nulls: usize,
        total: usize,
        min: i64,
        max: i64,
        has_default: bool,
        default_val: Option<i64>,
    },
    Float {
        nulls: usize,
        total: usize,
        min: f64,
        max: f64,
    },
    Boolean {
        nulls: usize,
        total: usize,
    },
    Null,
    Mixed(Vec<(String, usize)>, usize), // types with counts, total non-null
}

// ── public entry point ──

pub fn print_schema(data: &[u8], path: &Path) {
    let s = String::from_utf8_lossy(data);

    // Try JSON
    if let Ok(value) = serde_json::from_str::<Value>(&s) {
        let root = infer_value_schema(&value, &s);
        print_schema_tree("JSON", path, &root);
        return;
    }

    // Try TOML
    if let Ok(value) = toml::from_str::<Value>(&s) {
        let root = infer_value_schema(&value, &s);
        print_schema_tree("TOML", path, &root);
        return;
    }

    // Try YAML
    if let Ok(value) = serde_yaml::from_str::<Value>(&s) {
        let root = infer_value_schema(&value, &s);
        print_schema_tree("YAML", path, &root);
        return;
    }

    // Try CSV
    if looks_like_csv(&s) {
        let root = infer_csv_schema(&s);
        print_schema_tree("CSV", path, &root);
        return;
    }

    eprintln!(
        "ccat: --schema: unsupported format (try JSON, TOML, YAML, or CSV)"
    );
}

// ── schema inference from serde_json::Value ──

fn infer_value_schema(value: &Value, full_text: &str) -> SchemaNode {
    match value {
        Value::Object(map) => {
            let mut children = Vec::new();
            let depth = max_depth(value, 0);
            for (key, val) in map {
                children.push((key.clone(), infer_value_schema(val, full_text)));
            }
            // Sort children: objects first, then by name
            children.sort_by(|a, b| {
                let a_obj = matches!(a.1, SchemaNode::Object { .. });
                let b_obj = matches!(b.1, SchemaNode::Object { .. });
                b_obj
                    .cmp(&a_obj)
                    .then_with(|| a.0.cmp(&b.0))
            });
            SchemaNode::Object {
                total_keys: children.len(),
                depth,
                children,
            }
        }
        Value::Array(arr) => {
            let count = arr.len();
            let len_range = count_range(arr);
            if arr.is_empty() {
                SchemaNode::Array {
                    element: Box::new(SchemaNode::Null),
                    count: 0,
                    min_len: 0,
                    max_len: 0,
                }
            } else if arr.iter().all(|v| matches!(v, Value::Object(_))) {
                // Array of objects: merge schemas
                let merged = merge_object_schemas(arr);
                SchemaNode::Array {
                    element: Box::new(merged),
                    count,
                    min_len: len_range.0,
                    max_len: len_range.1,
                }
            } else if arr.iter().all(|v| matches!(v, Value::Array(_))) {
                // Array of arrays
                let inner_arr: Vec<&Value> = arr.iter().collect();
                let elem = infer_value_schema(&inner_arr[0], full_text);
                SchemaNode::Array {
                    element: Box::new(elem),
                    count,
                    min_len: len_range.0,
                    max_len: len_range.1,
                }
            } else {
                // Array of scalars or mixed
                let elem = infer_array_elements(arr);
                SchemaNode::Array {
                    element: Box::new(elem),
                    count,
                    min_len: len_range.0,
                    max_len: len_range.1,
                }
            }
        }
        Value::String(s) => {
            let detect = detect_string_format(s);
            SchemaNode::String {
                nulls: 0,
                total: 1,
                min_len: s.len(),
                max_len: s.len(),
                distinct: None,
                is_date: detect.date,
                is_url: detect.url,
                is_email: detect.email,
            }
        }
        Value::Number(n) => {
            if n.as_f64().map_or(true, |f| f.fract() != 0.0) {
                // Float
                let f = n.as_f64().unwrap_or(0.0);
                SchemaNode::Float {
                    nulls: 0,
                    total: 1,
                    min: f,
                    max: f,
                }
            } else if let Some(i) = n.as_i64() {
                SchemaNode::Integer {
                    nulls: 0,
                    total: 1,
                    min: i,
                    max: i,
                    has_default: false,
                    default_val: None,
                }
            } else {
                SchemaNode::Float {
                    nulls: 0,
                    total: 1,
                    min: n.as_f64().unwrap_or(0.0),
                    max: n.as_f64().unwrap_or(0.0),
                }
            }
        }
        Value::Bool(_) => SchemaNode::Boolean {
            nulls: 0,
            total: 1,
        },
        Value::Null => SchemaNode::Null,
    }
}

fn max_depth(value: &Value, depth: usize) -> usize {
    match value {
        Value::Object(map) => map
            .values()
            .map(|v| max_depth(v, depth + 1))
            .max()
            .unwrap_or(depth + 1),
        Value::Array(arr) => arr
            .iter()
            .map(|v| max_depth(v, depth + 1))
            .max()
            .unwrap_or(depth + 1),
        _ => depth + 1,
    }
}

fn count_range(arr: &[Value]) -> (usize, usize) {
    if arr.is_empty() {
        return (0, 0);
    }
    let mut min_len = usize::MAX;
    let mut max_len = 0;
    for item in arr {
        let len = match item {
            Value::Array(a) => a.len(),
            Value::String(s) => s.len(),
            Value::Object(m) => m.len(),
            _ => 1,
        };
        min_len = min_len.min(len);
        max_len = max_len.max(len);
    }
    (min_len, max_len)
}

fn merge_object_schemas(arr: &[Value]) -> SchemaNode {
    // Collect all keys across all objects
    let mut all_keys: Vec<String> = Vec::new();
    for v in arr {
        if let Value::Object(map) = v {
            for key in map.keys() {
                if !all_keys.contains(key) {
                    all_keys.push(key.clone());
                }
            }
        }
    }
    all_keys.sort();

    let mut children = Vec::new();
    for key in &all_keys {
        let values: Vec<&Value> = arr
            .iter()
            .filter_map(|v| v.get(key))
            .collect();
        let merged = if values.is_empty() {
            SchemaNode::Null
        } else if values.len() == 1 {
            infer_value_schema(values[0], "")
        } else {
            merge_scalar_schemas(&values)
        };
        children.push((key.clone(), merged));
    }

    let depth = children
        .iter()
        .filter_map(|(_, c)| match c {
            SchemaNode::Object { depth, .. } => Some(*depth),
            SchemaNode::Array { element, .. } => match element.as_ref() {
                SchemaNode::Object { depth, .. } => Some(*depth),
                _ => None,
            },
            _ => None,
        })
        .max()
        .unwrap_or(0)
        + 1;

    SchemaNode::Object {
        children,
        total_keys: all_keys.len(),
        depth,
    }
}

fn infer_array_elements(arr: &[Value]) -> SchemaNode {
    if arr.is_empty() {
        return SchemaNode::Null;
    }

    // Categorize all values
    let types = count_types(&arr.iter().collect::<Vec<&Value>>());
    if types.len() == 1 {
        // All same type — pick the first value's schema
        infer_value_schema(&arr[0], "")
    } else {
        // Mixed types
        let total: usize = types.values().sum();
        SchemaNode::Mixed(
            types
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            total,
        )
    }
}

fn merge_scalar_schemas(values: &[&Value]) -> SchemaNode {
    let nulls = values.iter().filter(|v| v.is_null()).count();
    let non_null: Vec<&&Value> = values.iter().filter(|v| !v.is_null()).collect();
    let total = values.len();

    if non_null.is_empty() {
        return SchemaNode::Null;
    }

    // Check all non-null types
    let types = count_types(&non_null.iter().map(|v| **v).collect::<Vec<&Value>>());

    if types.len() == 1 {
        let first = non_null[0];
        match first {
            Value::String(s) => {
                let (min_l, max_l) = string_len_range(&non_null);
                let detect = detect_string_format(s);
                SchemaNode::String {
                    nulls,
                    total,
                    min_len: min_l,
                    max_len: max_l,
                    distinct: None,
                    is_date: detect.date,
                    is_url: detect.url,
                    is_email: detect.email,
                }
            }
            Value::Number(n) => {
                if types.contains_key("float")
                    || n.as_f64().map_or(true, |f| f.fract() != 0.0)
                {
                    let (min_f, max_f) = float_range(&non_null);
                    SchemaNode::Float {
                        nulls,
                        total,
                        min: min_f,
                        max: max_f,
                    }
                } else {
                    let (min_i, max_i) = int_range(&non_null);
                    SchemaNode::Integer {
                        nulls,
                        total,
                        min: min_i,
                        max: max_i,
                        has_default: false,
                        default_val: None,
                    }
                }
            }
            Value::Bool(_) => SchemaNode::Boolean { nulls, total },
            _ => infer_value_schema(first, ""),
        }
    } else {
        SchemaNode::Mixed(
            types
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            total,
        )
    }
}

fn count_types(values: &[&Value]) -> std::collections::HashMap<&'static str, usize> {
    let mut counts = std::collections::HashMap::new();
    for v in values {
        let key = match v {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Number(n) => {
                if n.as_f64().map_or(true, |f| f.fract() != 0.0) {
                    "float"
                } else {
                    "int"
                }
            }
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        };
        *counts.entry(key).or_insert(0) += 1;
    }
    counts
}

fn string_len_range(values: &[&&Value]) -> (usize, usize) {
    let mut min = usize::MAX;
    let mut max = 0;
    for v in values {
        if let Value::String(s) = v {
            min = min.min(s.len());
            max = max.max(s.len());
        }
    }
    (min, max)
}

fn float_range(values: &[&&Value]) -> (f64, f64) {
    let mut min = f64::MAX;
    let mut max = f64::MIN;
    for v in values {
        if let Some(f) = v.as_f64() {
            min = min.min(f);
            max = max.max(f);
        }
    }
    (min, max)
}

fn int_range(values: &[&&Value]) -> (i64, i64) {
    let mut min = i64::MAX;
    let mut max = i64::MIN;
    for v in values {
        if let Some(i) = v.as_i64() {
            min = min.min(i);
            max = max.max(i);
        }
    }
    (min, max)
}

#[derive(Default)]
struct StringFormat {
    date: bool,
    url: bool,
    email: bool,
}

fn detect_string_format(s: &str) -> StringFormat {
    let mut fmt = StringFormat::default();
    // Date detection: ISO 8601 date/datetime
    if s.len() >= 10 {
        let chars: Vec<char> = s.chars().collect();
        if chars[4] == '-' && chars[7] == '-' {
            if let (Ok(y), Ok(m), Ok(d)) = (
                chars[0..4].iter().collect::<String>().parse::<i32>(),
                chars[5..7].iter().collect::<String>().parse::<u32>(),
                chars[8..10].iter().collect::<String>().parse::<u32>(),
            ) {
                if y >= 1900 && y <= 2100 && m >= 1 && m <= 12 && d >= 1 && d <= 31 {
                    fmt.date = true;
                }
            }
        }
    }
    // URL detection
    if s.starts_with("http://") || s.starts_with("https://") {
        fmt.url = true;
    }
    // Email detection  
    if s.contains('@') && s.contains('.') && !s.contains(' ') {
        let parts: Vec<&str> = s.split('@').collect();
        if parts.len() == 2 && !parts[0].is_empty() && parts[1].contains('.') {
            fmt.email = true;
        }
    }
    fmt
}

// ── CSV schema inference ──

fn looks_like_csv(s: &str) -> bool {
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 2 {
        return false;
    }
    let first_line = lines[0];
    if !first_line.contains(',') && !first_line.contains('\t') {
        return false;
    }
    true
}

fn infer_csv_schema(s: &str) -> SchemaNode {
    let delimiter = if s.contains('\t') { '\t' } else { ',' };
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();

    if lines.is_empty() {
        return SchemaNode::Null;
    }

    let headers: Vec<String> = if delimiter == '\t' {
        lines[0]
            .split('\t')
            .map(|c| c.trim().trim_matches('"').to_string())
            .collect()
    } else {
        let mut cols = Vec::new();
        let mut current = String::new();
        let mut in_quotes = false;
        for ch in lines[0].chars() {
            match ch {
                '"' => in_quotes = !in_quotes,
                ',' if !in_quotes => {
                    cols.push(current.trim().trim_matches('"').to_string());
                    current = String::new();
                }
                c => current.push(c),
            }
        }
        cols.push(current.trim().trim_matches('"').to_string());
        cols
    };

    let num_cols = headers.len();
    if num_cols == 0 {
        return SchemaNode::Null;
    }

    // Collect data per column
    let mut col_data: Vec<Vec<String>> = vec![Vec::new(); num_cols];
    let data_lines = if delimiter == '\t' {
        lines[1..]
            .iter()
            .map(|l| l.split('\t').map(|c| c.trim().trim_matches('"').to_string()).collect())
            .collect::<Vec<Vec<String>>>()
    } else {
        lines[1..]
            .iter()
            .map(|l| {
                let mut cols = Vec::new();
                let mut current = String::new();
                let mut in_quotes = false;
                for ch in l.chars() {
                    match ch {
                        '"' => in_quotes = !in_quotes,
                        ',' if !in_quotes => {
                            cols.push(current.trim().trim_matches('"').to_string());
                            current = String::new();
                        }
                        c => current.push(c),
                    }
                }
                cols.push(current.trim().trim_matches('"').to_string());
                cols
            })
            .collect()
    };

    for row in &data_lines {
        for (i, val) in row.iter().enumerate() {
            if i < num_cols {
                col_data[i].push(val.clone());
            }
        }
    }

    let total_rows = data_lines.len();
    let mut children = Vec::new();

    for (i, header) in headers.iter().enumerate() {
        let values = &col_data[i];
        let nulls = values.iter().filter(|v| v.is_empty() || v.as_str() == "null" || v.as_str() == "NULL" || v.as_str() == "N/A" || v.as_str() == "NA").count();

        let node = infer_csv_column_type(values, nulls);
        children.push((header.clone(), node));
    }

    SchemaNode::Object {
        total_keys: num_cols,
        depth: 1,
        children,
    }
}

fn infer_csv_column_type(values: &[String], nulls: usize) -> SchemaNode {
    let total = values.len();
    let non_null: Vec<&String> = values
        .iter()
        .filter(|v| !v.is_empty() && *v != "null" && *v != "NULL" && *v != "N/A" && *v != "NA")
        .collect();

    if non_null.is_empty() {
        return SchemaNode::Null;
    }

    // Count types
    let mut int_count = 0;
    let mut float_count = 0;
    let mut bool_count = 0;
    let int_values: std::cell::RefCell<Vec<i64>> = std::cell::RefCell::new(Vec::new());
    let float_values: std::cell::RefCell<Vec<f64>> = std::cell::RefCell::new(Vec::new());

    for v in &non_null {
        let trimmed = v.trim();
        if let Ok(i) = trimmed.parse::<i64>() {
            int_count += 1;
            int_values.borrow_mut().push(i);
        } else if let Ok(f) = trimmed.parse::<f64>() {
            float_count += 1;
            float_values.borrow_mut().push(f);
        } else if trimmed.eq_ignore_ascii_case("true")
            || trimmed.eq_ignore_ascii_case("false")
            || trimmed == "1"
            || trimmed == "0"
            || trimmed.eq_ignore_ascii_case("yes")
            || trimmed.eq_ignore_ascii_case("no")
        {
            bool_count += 1;
        }
    }

    let str_count = non_null.len() - int_count - float_count - bool_count;

    // Determine best type
    let distinct = {
        let mut uniq: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for v in non_null.iter() {
            uniq.insert(v.as_str());
        }
        Some(uniq.len())
    };

    // Check if it looks like a date
    let looks_like_date = non_null
        .iter()
        .any(|v| {
            let t = v.trim();
            t.len() >= 10
                && t.as_bytes().get(4) == Some(&b'-')
                && t.as_bytes().get(7) == Some(&b'-')
        });

    let looks_like_url = non_null
        .iter()
        .any(|v| v.starts_with("http://") || v.starts_with("https://"));

    if int_count > 0 && float_count == 0 && bool_count == 0 && str_count == 0 {
        let iv = int_values.borrow();
        let min = *iv.iter().min().unwrap_or(&0);
        let max = *iv.iter().max().unwrap_or(&0);
        SchemaNode::Integer {
            nulls,
            total,
            min,
            max,
            has_default: false,
            default_val: None,
        }
    } else if float_count > 0 && str_count == 0 {
        let fv = float_values.borrow();
        let min = fv.iter().cloned().fold(f64::MAX, f64::min);
        let max = fv.iter().cloned().fold(f64::MIN, f64::max);
        SchemaNode::Float {
            nulls,
            total,
            min,
            max,
        }
    } else if bool_count > 0 && str_count == 0 && int_count == 0 && float_count == 0 {
        SchemaNode::Boolean { nulls, total }
    } else if looks_like_date {
        SchemaNode::String {
            nulls,
            total,
            min_len: 10,
            max_len: non_null.iter().map(|s| s.len()).max().unwrap_or(10),
            distinct,
            is_date: true,
            is_url: false,
            is_email: false,
        }
    } else {
        let min_len = non_null.iter().map(|s| s.len()).min().unwrap_or(0);
        let max_len = non_null.iter().map(|s| s.len()).max().unwrap_or(0);
        SchemaNode::String {
            nulls,
            total,
            min_len,
            max_len,
            distinct,
            is_date: false,
            is_url: looks_like_url,
            is_email: false,
        }
    }
}

// ── tree printing ──

fn print_schema_tree(format_name: &str, path: &Path, root: &SchemaNode) {
    let stdout = std::io::stdout();
    let mut h = stdout.lock();

    let path_str = path.to_string_lossy();
    let _ = writeln!(
        h,
        "\x1b[1m{T_ROOT}\x1b[33m{}\x1b[0m \x1b[2m— {}\x1b[0m",
        format_name,
        path_str
    );

    match root {
        SchemaNode::Object {
            children,
            total_keys,
            depth,
        } => {
            for (i, (name, child)) in children.iter().enumerate() {
                let is_last = i == children.len() - 1;
                print_node(&mut h, name, child, "", is_last);
            }
            let _ = writeln!(
                h,
                "\x1b[2m{} {} key{}, {} level{}\x1b[0m",
                if children.is_empty() { "" } else { " " },
                total_keys,
                if *total_keys == 1 { "" } else { "s" },
                depth,
                if *depth == 1 { "" } else { "s" },
            );
        }
        _ => {
            print_node(&mut h, "value", root, "", true);
        }
    }
}

fn print_node(
    h: &mut std::io::StdoutLock,
    name: &str,
    node: &SchemaNode,
    prefix: &str,
    is_last: bool,
) {
    let connector = if is_last { T_LAST } else { T_BRANCH };
    let child_prefix = if is_last { T_SPACE } else { T_VLINE };

    let label = node_label(node);
    let _ = writeln!(
        h,
        "{}{}\x1b[33m{}\x1b[0m: {}",
        prefix, connector, name, label
    );

    if let SchemaNode::Object { children, .. } = node {
        for (i, (child_name, child)) in children.iter().enumerate() {
            let child_is_last = i == children.len() - 1;
            print_node(h, child_name, child, &format!("{}{}", prefix, child_prefix), child_is_last);
        }
    }

    if let SchemaNode::Array { element, count, min_len, max_len, .. } = node {
        if let SchemaNode::Object { children, .. } = element.as_ref() {
            for (i, (child_name, child)) in children.iter().enumerate() {
                let child_is_last = i == children.len() - 1;
                print_node(h, child_name, child, &format!("{}{}", prefix, child_prefix), child_is_last);
            }
        }
    }
}

fn node_label(node: &SchemaNode) -> String {
    match node {
        SchemaNode::Object { .. } => String::new(),
        SchemaNode::Array {
            element,
            count,
            min_len,
            max_len,
        } => {
            let elem_label = element_label(element);
            let len_info = if *count > 0 {
                if min_len == max_len {
                    format!(" ×{}", count)
                } else {
                    format!(" ×{}", count)
                }
            } else {
                String::new()
            };
            format!("\x1b[2m[{}]{}\x1b[0m{}", elem_label, if *count > 0 { "" } else { "" }, len_info)
        }
        SchemaNode::String {
            nulls,
            total,
            min_len,
            max_len,
            distinct,
            is_date,
            is_url,
            is_email,
        } => {
            let mut parts = vec![format!("\x1b[32mstring\x1b[0m")];
            if *is_date {
                parts.push("\x1b[35mdate\x1b[0m".to_string());
            }
            if *is_url {
                parts.push("\x1b[35murl\x1b[0m".to_string());
            }
            if *is_email {
                parts.push("\x1b[35memail\x1b[0m".to_string());
            }
            parts.push(format!("\x1b[2m{}–{} chars\x1b[0m", min_len, max_len));
            if let Some(d) = distinct {
                let ratio = if *total > 0 {
                    (*d as f64 / *total as f64 * 100.0) as u32
                } else {
                    0
                };
                parts.push(format!("\x1b[2m{} distinct ({}%)\x1b[0m", d, ratio));
            }
            if *nulls > 0 {
                parts.push(format!("\x1b[2m{} null\x1b[0m", nulls));
            }
            parts.join(" ")
        }
        SchemaNode::Integer {
            nulls,
            total,
            min,
            max,
            has_default: _,
            default_val: _,
        } => {
            let mut parts = vec![];
            if *min == *max {
                parts.push(format!("\x1b[95m{} (integer)\x1b[0m", min));
            } else {
                parts.push(format!("\x1b[95m{}–{}\x1b[0m", min, max));
                parts.push("\x1b[2minteger\x1b[0m".to_string());
            }
            if *nulls > 0 {
                parts.push(format!("\x1b[2m{} null\x1b[0m", nulls));
            }
            parts.join(" ")
        }
        SchemaNode::Float {
            nulls,
            total: _,
            min,
            max,
        } => {
            let mut parts = vec![];
            if (*min - *max).abs() < f64::EPSILON {
                parts.push(format!("\x1b[95m{} (float)\x1b[0m", min));
            } else {
                parts.push(format!("\x1b[95m{}–{}\x1b[0m", min, max));
                parts.push("\x1b[2mfloat\x1b[0m".to_string());
            }
            if *nulls > 0 {
                parts.push(format!("\x1b[2m{} null\x1b[0m", nulls));
            }
            parts.join(" ")
        }
        SchemaNode::Boolean { nulls, total: _ } => {
            let mut parts = vec![format!("\x1b[36mboolean\x1b[0m")];
            if *nulls > 0 {
                parts.push(format!("\x1b[2m{} null\x1b[0m", nulls));
            }
            parts.join(" ")
        }
        SchemaNode::Null => "\x1b[2mnull\x1b[0m".to_string(),
        SchemaNode::Mixed(types, total) => {
            let parts: Vec<String> = types
                .iter()
                .map(|(t, c)| {
                    let pct = if *total > 0 {
                        *c as f64 / *total as f64 * 100.0
                    } else {
                        0.0
                    };
                    format!("{} ({:.0}%)", t, pct)
                })
                .collect();
            format!("\x1b[31m({})\x1b[0m", parts.join(" | "))
        }
    }
}

fn element_label(node: &SchemaNode) -> String {
    match node {
        SchemaNode::Object { total_keys, .. } => format!("object {} keys", total_keys),
        SchemaNode::String { .. } => "string".to_string(),
        SchemaNode::Integer { .. } => "integer".to_string(),
        SchemaNode::Float { .. } => "float".to_string(),
        SchemaNode::Boolean { .. } => "boolean".to_string(),
        SchemaNode::Null => "null".to_string(),
        SchemaNode::Mixed(types, _) => types
            .iter()
            .map(|(t, _)| t.as_str())
            .collect::<Vec<&str>>()
            .join("|"),
        SchemaNode::Array { .. } => "array".to_string(),
    }
}

// ── tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_schema_simple() {
        let json = br#"{"name": "Alice", "age": 30, "active": true}"#;
        let value: Value = serde_json::from_slice(json).unwrap();
        let schema = infer_value_schema(&value, "");
        match &schema {
            SchemaNode::Object { children, total_keys, .. } => {
                assert_eq!(*total_keys, 3);
                assert_eq!(children.len(), 3);
                // Sorted alphabetically: active, age, name
                assert_eq!(children[0].0, "active");
                assert_eq!(children[1].0, "age");
                assert_eq!(children[2].0, "name");
            }
            _ => panic!("expected Object"),
        }
    }

    #[test]
    fn test_json_schema_nested() {
        let json = br#"{"user": {"name": "Bob", "scores": [85, 92, 78]}, "meta": {"version": 2}}"#;
        let value: Value = serde_json::from_slice(json).unwrap();
        let schema = infer_value_schema(&value, "");
        match &schema {
            SchemaNode::Object { children, total_keys, .. } => {
                assert_eq!(*total_keys, 2);
                // Sorted: meta, user (both are objects, alphabetical)
                assert_eq!(children[0].0, "meta");
                assert_eq!(children[1].0, "user");
                // meta should be Object with 1 field
                match &children[0].1 {
                    SchemaNode::Object { children, .. } => {
                        assert_eq!(children.len(), 1); // version
                    }
                    _ => panic!("expected Object for meta"),
                }
            }
            _ => panic!("expected Object"),
        }
    }

    #[test]
    fn test_json_schema_array_of_objects() {
        let json = br#"[
            {"name": "Alice", "age": 30},
            {"name": "Bob", "age": 25},
            {"name": "Charlie", "age": 35}
        ]"#;
        let value: Value = serde_json::from_slice(json).unwrap();
        let schema = infer_value_schema(&value, "");
        match &schema {
            SchemaNode::Array { element, count, .. } => {
                assert_eq!(*count, 3);
                match element.as_ref() {
                    SchemaNode::Object { children, .. } => {
                        assert_eq!(children.len(), 2);
                        assert!(children.iter().any(|(n, _)| n == "name"));
                        assert!(children.iter().any(|(n, _)| n == "age"));
                    }
                    _ => panic!("expected Object element"),
                }
            }
            _ => panic!("expected Array"),
        }
    }

    #[test]
    fn test_json_schema_mixed_array() {
        let json = br#"[1, "hello", true, null]"#;
        let value: Value = serde_json::from_slice(json).unwrap();
        let schema = infer_value_schema(&value, "");
        match &schema {
            SchemaNode::Array { element, count, .. } => {
                assert_eq!(*count, 4);
                match element.as_ref() {
                    SchemaNode::Mixed(types, total) => {
                        assert!(*total > 0);
                        assert!(types.iter().any(|(t, _)| *t == "int" || *t == "string" || *t == "bool" || *t == "null"));
                    }
                    _ => panic!("expected Mixed element for heterogeneous array"),
                }
            }
            _ => panic!("expected Array"),
        }
    }

    #[test]
    fn test_csv_schema_basic() {
        let csv = "name,age,active\nAlice,30,true\nBob,25,false\nCharlie,35,true\n";
        let schema = infer_csv_schema(csv);
        match &schema {
            SchemaNode::Object { children, total_keys, .. } => {
                assert_eq!(*total_keys, 3);
                // Check column types
                for (name, node) in children {
                    match name.as_str() {
                        "name" => assert!(matches!(node, SchemaNode::String { .. })),
                        "age" => assert!(matches!(node, SchemaNode::Integer { .. })),
                        "active" => assert!(matches!(node, SchemaNode::Boolean { .. })),
                        _ => panic!("unexpected column: {}", name),
                    }
                }
            }
            _ => panic!("expected Object"),
        }
    }

    #[test]
    fn test_csv_schema_with_nulls() {
        let csv = "name,age\nAlice,30\nBob,\nCharlie,35\n";
        let schema = infer_csv_schema(csv);
        match &schema {
            SchemaNode::Object { children, .. } => {
                for (name, node) in children {
                    match name.as_str() {
                        "name" => assert!(matches!(node, SchemaNode::String { .. })),
                        "age" => {
                            if let SchemaNode::Integer { nulls, .. } = node {
                                assert_eq!(*nulls, 1);
                            } else {
                                panic!("expected Integer for age")
                            }
                        }
                        _ => panic!("unexpected column: {}", name),
                    }
                }
            }
            _ => panic!("expected Object"),
        }
    }

    #[test]
    fn test_detect_string_format_date() {
        let fmt = detect_string_format("2024-01-15");
        assert!(fmt.date);
        assert!(!fmt.url);
        assert!(!fmt.email);
    }

    #[test]
    fn test_detect_string_format_url() {
        let fmt = detect_string_format("https://example.com/path");
        assert!(fmt.url);
    }

    #[test]
    fn test_detect_string_format_email() {
        let fmt = detect_string_format("user@example.com");
        assert!(fmt.email);
    }

    #[test]
    fn test_csv_type_inference() {
        let values = vec!["10".to_string(), "20".to_string(), "30".to_string()];
        let node = infer_csv_column_type(&values, 0);
        match node {
            SchemaNode::Integer { min, max, .. } => {
                assert_eq!(min, 10);
                assert_eq!(max, 30);
            }
            _ => panic!("expected Integer"),
        }
    }

    #[test]
    fn test_empty_json() {
        let value: Value = serde_json::from_str("{}").unwrap();
        let schema = infer_value_schema(&value, "");
        match &schema {
            SchemaNode::Object { children, total_keys, .. } => {
                assert_eq!(children.len(), 0);
                assert_eq!(*total_keys, 0);
            }
            _ => panic!("expected Object"),
        }
    }

    #[test]
    fn test_null_values() {
        let json = br#"{"a": null, "b": "hello"}"#;
        let value: Value = serde_json::from_slice(json).unwrap();
        let schema = infer_value_schema(&value, "");
        match &schema {
            SchemaNode::Object { children, .. } => {
                assert_eq!(children.len(), 2);
                // Sorted alphabetically: a (null), b (string)
                assert!(matches!(children[0].1, SchemaNode::Null));
                assert!(matches!(children[1].1, SchemaNode::String { .. }));
            }
            _ => panic!("expected Object"),
        }
    }
}
