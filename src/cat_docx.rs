use docx_rs::*;

/// Render .docx word document to terminal with basic formatting.
pub fn cat_docx(data: &[u8]) {
    match read_docx(data) {
        Ok(docx) => {
            for child in &docx.document.children {
                if let DocumentChild::Paragraph(paragraph) = child {
                    print_paragraph(paragraph);
                }
            }
            // Print tables separately
            for child in &docx.document.children {
                if let DocumentChild::Table(table) = child {
                    print_table(table);
                }
            }
        }
        Err(e) => {
            eprintln!("ccat: docx parse error: {e:?}");
        }
    }
}

fn print_paragraph(para: &Paragraph) {
    if para.children.is_empty() {
        println!();
        return;
    }

    for child in &para.children {
        if let ParagraphChild::Run(run) = child {
            print_run(run);
        }
    }
    println!();
}

fn print_run(run: &Run) {
    let mut open = String::new();
    let mut close = String::new();

    if run.run_property.bold.is_some() {
        open.push_str("\x1b[1m");
        close = format!("\x1b[22m{close}");
    }
    if run.run_property.italic.is_some() {
        open.push_str("\x1b[3m");
        close = format!("\x1b[23m{close}");
    }
    if run.run_property.underline.is_some() {
        open.push_str("\x1b[4m");
        close = format!("\x1b[24m{close}");
    }
    if run.run_property.strike.is_some() {
        open.push_str("\x1b[9m");
        close = format!("\x1b[29m{close}");
    }

    // Color: use Debug output to get the hex value
    if let Some(color) = &run.run_property.color {
        let debug_str = format!("{color:?}");
        // Debug format: Color { val: "FF0000" }
        if let Some(val_start) = debug_str.find("val: \"") {
            let start = val_start + 6;
            if let Some(val_end) = debug_str[start..].find('"') {
                let hex = &debug_str[start..start + val_end];
                if hex.len() == 6 {
                    if let (Ok(r), Ok(g), Ok(b)) = (
                        u8::from_str_radix(&hex[0..2], 16),
                        u8::from_str_radix(&hex[2..4], 16),
                        u8::from_str_radix(&hex[4..6], 16),
                    ) {
                        open.push_str(&format!("\x1b[38;2;{r};{g};{b}m"));
                        close = format!("\x1b[39m{close}");
                    }
                }
            }
        }
    }

    if !open.is_empty() {
        print!("{open}");
    }

    for run_child in &run.children {
        if let RunChild::Text(text) = run_child {
            print!("{}", text.text);
        } else if let RunChild::Tab(_) = run_child {
            print!("\t");
        }
    }

    if !close.is_empty() {
        print!("{close}");
    }
}

fn print_table(table: &Table) {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut widths: Vec<usize> = Vec::new();

    for child in &table.rows {
        let TableChild::TableRow(row) = child;
        let mut row_cells = Vec::new();
        for cell in &row.cells {
            let TableRowChild::TableCell(cell) = cell;
            let mut cell_text = String::new();
            for p_child in &cell.children {
                if let TableCellContent::Paragraph(para) = p_child {
                    cell_text.push_str(&para.raw_text());
                }
            }
            row_cells.push(cell_text);
        }
        // Update widths
        while widths.len() < row_cells.len() {
            widths.push(0);
        }
        for (i, text) in row_cells.iter().enumerate() {
            widths[i] = widths[i].max(text.len());
        }
        rows.push(row_cells);
    }

    if rows.is_empty() {
        return;
    }

    // Print table
    let total = widths.iter().sum::<usize>() + widths.len() * 3 + 1;
    let line: String = std::iter::repeat('─').take(total).collect();

    println!("\x1b[2m{line}\x1b[0m");
    for row in &rows {
        print!("\x1b[2m│\x1b[0m ");
        for (i, cell) in row.iter().enumerate() {
            print!("{cell}");
            if i + 1 < widths.len() {
                let padding = widths[i].saturating_sub(cell.len());
                print!("{} \x1b[2m│\x1b[0m ", " ".repeat(padding));
            } else if widths.len() > 0 {
                let padding = widths[i].saturating_sub(cell.len());
                print!("{}", " ".repeat(padding));
            }
        }
        println!(" \x1b[2m│\x1b[0m");
        println!("\x1b[2m{line}\x1b[0m");
    }
    println!();
}
