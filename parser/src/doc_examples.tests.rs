use super::*;
use crate::Pratt;

#[test]
fn fence_tags_pick_the_mode() {
    assert_eq!(fence_mode(""), Some(ExampleMode::Run));
    assert_eq!(fence_mode("coil"), Some(ExampleMode::Run));
    assert_eq!(fence_mode("ignore"), Some(ExampleMode::Ignore));
    assert_eq!(fence_mode("coil,no_run"), Some(ExampleMode::NoRun));
    assert_eq!(fence_mode("should_panic"), Some(ExampleMode::ShouldPanic));
    assert_eq!(fence_mode("coil, should_panic"), Some(ExampleMode::ShouldPanic));
    // `ignore` wins over the others; `no_run` over `should_panic`.
    assert_eq!(fence_mode("should_panic,ignore"), Some(ExampleMode::Ignore));
    assert_eq!(fence_mode("should_panic,no_run"), Some(ExampleMode::NoRun));
    assert_eq!(fence_mode("text"), None);
    assert_eq!(fence_mode("json"), None);
    assert_eq!(fence_mode("coil,text"), None);
}

#[test]
fn hidden_lines_are_hash_space_or_a_lone_hash() {
    assert!(is_hidden_line("# use tests::assert;"));
    assert!(is_hidden_line("    # let x = 1;"));
    assert!(is_hidden_line("#"));
    assert!(!is_hidden_line("#[derive(Show)]"));
    assert!(!is_hidden_line("#x"));
    assert!(!is_hidden_line("let x = 1; # not hidden"));
    assert_eq!(unhide("# use tests::assert;"), "use tests::assert;");
    assert_eq!(unhide("    # let x = 1;"), "    let x = 1;");
    assert_eq!(unhide("#"), "");
    assert_eq!(unhide("#[derive(Show)]"), "#[derive(Show)]");
}

#[test]
fn examples_are_the_coil_fences() {
    let docs = [
        "Adds one.",
        "",
        "```",
        "# use tests::assert;",
        "assert!(add(1) == 2);",
        "```",
        "",
        "```text",
        "not code",
        "```",
        "",
        "```coil,no_run",
        "loop_forever();",
        "```",
        "```ignore",
        "broken(",
        "```",
        "```should_panic",
        "panic(\"x\");",
    ];
    let found = examples(&docs);
    assert_eq!(found.len(), 4);
    assert_eq!(found[0].mode, ExampleMode::Run);
    assert_eq!(found[0].fence, 2);
    assert_eq!(found[0].lines, vec!["# use tests::assert;", "assert!(add(1) == 2);"]);
    assert_eq!(found[1].mode, ExampleMode::NoRun);
    assert_eq!(found[1].fence, 11);
    assert_eq!(found[2].mode, ExampleMode::Ignore);
    // An unclosed fence runs to the end.
    assert_eq!(found[3].mode, ExampleMode::ShouldPanic);
    assert_eq!(found[3].lines, vec!["panic(\"x\");"]);
}

#[test]
fn indented_fences_count() {
    let docs = ["  ```", "  let x = 1;", "  ```"];
    let found = examples(&docs);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].lines, vec!["  let x = 1;"]);
}

#[test]
fn program_hoists_uses_and_wraps_in_main() {
    let example = DocExample {
        mode: ExampleMode::Run,
        fence: 0,
        lines: vec![
            "# use tests::assert;",
            "let v = to_bytes(\"hi\");",
            "use string::{",
            "    to_bytes,",
            "};",
            "",
            "assert!(v.len() == 2);",
        ],
    };
    assert_eq!(
        example.program(),
        "use tests::assert;\nuse string::{\nto_bytes,\n};\nfn main() {\n    let v = to_bytes(\"hi\");\n\n    assert!(v.len() == 2);\n}\n"
    );
}

#[test]
fn program_keeps_an_example_with_its_own_main() {
    let example = DocExample {
        mode: ExampleMode::Run,
        fence: 0,
        lines: vec!["# use tests::assert;", "fn helper() -> int { return 1; }", "fn main() {", "    assert!(helper() == 1);", "}"],
    };
    assert_eq!(
        example.program(),
        "use tests::assert;\nfn helper() -> int { return 1; }\nfn main() {\n    assert!(helper() == 1);\n}\n"
    );
    assert!(declares_main("pub fn main() {"));
    assert!(declares_main("fn  main ()"));
    assert!(!declares_main("fn mainly() {"));
    assert!(!declares_main("// fn main() is not needed"));
}

#[test]
fn documented_items_name_owners() {
    let source = "/// f\n\
/// ```\n\
/// f();\n\
/// ```\n\
fn f() {}\n\
\n\
/// A point.\n\
class Point {\n\
    /// x.\n\
    pub x: int,\n\
    pub y: int,\n\
}\n\
\n\
impl Point {\n\
    /// Norm.\n\
    pub fn norm() -> int { return 0; }\n\
}\n\
\n\
enum Shape {\n\
    /// Round.\n\
    Circle(int),\n\
    Square,\n\
}\n\
\n\
trait Area<T> {\n\
    /// Area of it.\n\
    fn area(T t) -> int {}\n\
}\n\
\n\
/// Twice.\n\
macro twice(Expr e) -> Code { return lit(e.text); }\n\
\n\
fn undocumented() {}\n";
    let ast = Pratt::default().parse(source).expect("parses");
    let items = documented_items(&ast);
    let paths: Vec<&str> = items.iter().map(|i| i.path.as_str()).collect();
    assert_eq!(paths, vec!["f", "Point", "Point::x", "Point::norm", "Shape::Circle", "Area::area", "twice"]);
    assert_eq!(line_of(source, items[0].docs[1]), Some(2));
    assert_eq!(line_of(source, items[2].docs[0]), Some(9));
    let elsewhere = String::from("elsewhere");
    assert_eq!(line_of(source, &elsewhere), None);
}
