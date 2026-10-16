// Expected: E0300 — `%s` takes a `string`; an `Option` value is not one.
fn main() {
    let s = string::format("%s", Option::Some(1));
}
