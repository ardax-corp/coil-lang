//! `Pratt::parse_at(text, offset)` is `Pratt::parse` of `text` behind
//! `offset` bytes of whitespace: the same tree, spans and parse errors
//! (macro output is parsed this way, coil-lang#1398).

use std::path::{Path, PathBuf};

use parser::Pratt;

const OFFSET: usize = 1000;

fn check(text: &str, what: &str) {
    let padded = format!("{}{text}", " ".repeat(OFFSET));
    let want = Pratt::default().parse(&padded);
    let got = Pratt::default().parse_at(text, OFFSET);
    assert_eq!(got, want, "{what}");
}

fn hy_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            hy_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "hy") {
            out.push(path);
        }
    }
}

/// The `.hy` files of corpus dirs that cover macros, contracts, effects,
/// patterns and parse errors (the whole corpus is slow in a debug build).
#[test]
fn parse_at_matches_padded_parse_on_the_corpus() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut files = vec![repo.join("tests/generics/multiple_bounds.hy")];
    for dir in ["macros", "contracts", "effects", "patterns", "syntax", "errors"] {
        hy_files(&repo.join("tests").join(dir), &mut files);
    }
    files.sort();
    assert!(files.len() > 100, "corpus not found under {}", repo.display());
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap();
        check(&text, &file.display().to_string());
    }
}

#[test]
fn parse_at_matches_padded_parse_on_errors() {
    for text in [
        "fn __coil_m() {\nreturn (\n1 +\n);\n}",
        "fn f(n: int) {}",
        "impl {\n}",
        "class C { x int }",
        "fn f() { let x = ; }",
        "",
        "fn",
    ] {
        check(text, text);
    }
}
