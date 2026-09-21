// COI-404: Result<string, string> from a real module that shares short
// names with a sibling (`to_lower`). Tiny same-file copies already passed.
use coi404_textmod::{to_lower, replace_mark};

test("module to_lower match keeps Ok payload") {
    let low = match to_lower("AbC") {
        Result::Ok(s) => s,
        Result::Err(_) => panic "lower",
    };
    assert(low == "abc")?;
}

test("module replace-like question keeps Ok payload") {
    assert(replace_mark("AbC")? == "AbCx")?;
}
