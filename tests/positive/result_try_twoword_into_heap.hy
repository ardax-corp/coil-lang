// `?` from two-word `Result<int, E>` into heap-heap `Result<string, E>`
// must return Err, not an aligned boxed Result that looks like Ok.
enum E {
    Bad,
}

fn two_word_err() -> Result<int, E> {
    raise E::Bad;
}

fn heap_ok_via_q() -> Result<string, E> {
    two_word_err()?;
    return "x";
}

test("two-word Err ? into heap Result is Err") {
    assert(match heap_ok_via_q() {
        Result::Ok(_) => false,
        Result::Err(_) => true,
    }, "q")?;
}
