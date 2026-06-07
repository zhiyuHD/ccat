use syntect::html::highlighted_html_for_string;
use syntect::parsing::SyntaxSet;
use syntect::highlighting::ThemeSet;

fn main() {
    let ss = SyntaxSet::load_defaults_newlines();
    let ts = ThemeSet::load_defaults();
    let theme = &ts.themes["base16-ocean.dark"];
    
    let code = "fn hello() {\n    println!(\"world!\");\n}\n";
    match highlighted_html_for_string(code, &ss, ss.find_syntax_by_extension("rs").unwrap(), theme) {
        Ok(html) => println!("HTML output (first 300 chars):\n{}", &html[..html.len().min(300)]),
        Err(e) => eprintln!("Error: {e}"),
    }
}
