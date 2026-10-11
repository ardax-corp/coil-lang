// Expected: E0300 — after `%%`, `%s` still requires a string.
use string::format;
fn main() {
    let s = format("%%%s", 42);
}
