// `symbol::fn_key()` keys a function by its module path too: a function and
// its namesake in another module get different keys, and an imported
// function keeps its own key wherever it is called (or inlined).
use tests::assert;
use symbol::fn_key;
use fn_key_mod::keyed;

fn plain(int x) -> int {
    return fn_key();
}

test("a namesake in another module has another key") {
    assert!(plain(1) != fn_key_mod::plain(1));
    assert!(fn_key_mod::plain(1) == fn_key_mod::plain(2));
}

test("an imported function keeps its key") {
    assert!(keyed() == keyed());
    assert!(keyed() != fn_key_mod::plain(1));
    assert!(keyed() != fn_key());
}
