use syntect::highlighting::ThemeSet;
#[test]
fn list_themes() {
    let ts = ThemeSet::load_defaults();
    let mut themes: Vec<_> = ts.themes.keys().collect();
    themes.sort();
    for t in &themes {
        println!("{}", t);
    }
    assert!(themes.len() > 20);
}
