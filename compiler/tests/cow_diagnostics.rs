//! Copy-on-write warnings: E0141 at a write that copies, with the borrow
//! that avoids it, E0142 at a write to a temporary copy, and E0143 at an
//! assignment to a `use (mut x)` capture.

use compiler::Pipeline;
use reporting::ErrorCode;

/// `(code, message, help)` of each copy-on-write warning in `src`.
fn cow_notes(src: &str) -> Vec<(ErrorCode, String, String)> {
    let mut pipeline = Pipeline::new();
    pipeline.compile_src(src).expect("compile");
    pipeline
        .messages()
        .iter()
        .filter(|m| matches!(m.code(), Some(ErrorCode::CowCopy | ErrorCode::CowDroppedWrite | ErrorCode::CaptureAssign)))
        .map(|m| (m.code().unwrap(), m.message().to_string(), m.help().clone().unwrap_or_default()))
        .collect()
}

const ITEM: &str = "
class Item {
    pub n: int,
}

impl Item {
    pub fn bump() {
        self.n += 1;
    }
}
";

#[test]
fn let_copy_suggests_a_let_borrow() {
    let src = format!("{ITEM}
fn main() {{
    let a = new Item(1);
    let b = a;
    b.n = 2;
    if a.n + b.n != 3 {{
        panic \"copy\";
    }}
}}
");
    let notes = cow_notes(&src);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0].0, ErrorCode::CowCopy);
    assert!(notes[0].2.contains("`let b = mut a`"), "{notes:?}");
}

// #2073: a `for` binding is borrowed with `for mut`, not `let … = mut`.
#[test]
fn for_copy_suggests_for_mut() {
    let src = format!("{ITEM}
fn main() {{
    let items: Vec<Item> = [new Item(1)];
    for it in items {{
        it.n = 9;
    }}
    if items[0].n != 1 {{
        panic \"copy\";
    }}
}}
");
    let notes = cow_notes(&src);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].2.contains("`for mut it in items`"), "{notes:?}");
}

#[test]
fn write_to_a_returned_element_is_a_dropped_write() {
    let src = format!("{ITEM}
fn first(Vec<Item> v) -> Item {{
    return v[0];
}}

fn main() {{
    let items: Vec<Item> = [new Item(1)];
    first(items).bump();
    if items[0].n != 1 {{
        panic \"dropped\";
    }}
}}
");
    let notes = cow_notes(&src);
    assert!(notes.iter().any(|n| n.0 == ErrorCode::CowDroppedWrite && n.1.contains("`.bump()`")), "{notes:?}");
}

// #2072: `use (mut n)` shares `n`'s object; assigning `n` stays in the
// closure.
#[test]
fn assigning_a_mut_capture_warns() {
    let notes = cow_notes("
fn main() {
    let n = 0;
    let f = fn () use (mut n) { n += 1; };
    f();
    if n != 0 {
        panic \"write-back\";
    }
}
");
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0].0, ErrorCode::CaptureAssign);
    assert!(notes[0].2.contains("`Ref<T>`"), "{notes:?}");
}

#[test]
fn writing_through_a_mut_capture_does_not_warn() {
    let src = format!("{ITEM}
fn main() {{
    let it = new Item(1);
    let f = fn () use (mut it) {{ it.n += 1; }};
    f();
    if it.n != 2 {{
        panic \"borrow\";
    }}
}}
");
    assert!(cow_notes(&src).is_empty(), "{:?}", cow_notes(&src));
}

// #2155: a `Stream` is an opaque handle, so passing a captured one on is no copy.
#[test]
fn captured_stream_passed_on_does_not_warn() {
    let src = "
use io::{close};
fn takes(Stream s, Stream -> unit h) { h(s); }
fn run(Stream l) {
    let g = fn () use (l) { takes(l, fn (Stream x) { let _ = close(x); }); };
    g();
}
fn main() {}
";
    let notes = cow_notes(src);
    assert!(notes.is_empty(), "{notes:?}");
}
