// `Root<T>` and `Weak<T>` handles are one host word each, moved between
// locals and passed to the `gc` natives.
use gc::{get, root, unroot, upgrade, weak};

fn pinned(string s) -> string {
    let r = root(s);
    let kept = r;
    let got = match get(kept) {
        Option::Some(v) => v,
        Option::None => "gone",
    };
    let _ = unroot(kept);
    return got;
}

fn alive(string s) -> bool {
    let w = weak(s);
    return match upgrade(w) {
        Option::Some(_) => true,
        Option::None => false,
    };
}

test("root then get and unroot") {
    assert(pinned("pin") == "pin")?;
}

test("weak upgrade of a live string") {
    let s = "live";
    assert(alive(s))?;
}
