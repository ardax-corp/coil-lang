// `coil test` runs the examples in `///` docs as doc-tests: those of this
// file and of the project source it imports (examples/src/doc_tests_demo.hy).
use tests::assert;
use doc_tests_demo::inc;

/// Hidden `# ` lines run; `use` lines are hoisted out of the `fn main` the
/// example is wrapped in.
///
/// ```
/// # use tests::assert;
/// let s = format("%v-%v", 1, 2);
/// use string::{format};
/// assert!(s == "1-2");
/// ```
fn hoisted() {}

/// An example with its own `main` is not wrapped.
///
/// ```coil
/// use tests::assert;
///
/// fn twice(int x) -> int {
///     return x * 2;
/// }
///
/// fn main() {
///     assert!(twice(2) == 4);
/// }
/// ```
fn own_main() {}

/// `should_panic` passes when the example panics, and a failed `assert!`
/// (an `Err` reaching the end of `main`) is a panic.
///
/// ```should_panic
/// panic("expected");
/// ```
///
/// ```should_panic
/// # use tests::assert;
/// assert!(1 == 2, "expected");
/// ```
fn panics() {}

/// `no_run` compiles the example without running it; `ignore` and other
/// languages are not compiled.
///
/// ```no_run
/// panic("never runs");
/// ```
///
/// ```ignore
/// this is not coil(
/// ```
///
/// ```text
/// neither is this(
/// ```
fn skipped() {}

test("the imported module works") {
    assert!(inc(1) == 2);
}
