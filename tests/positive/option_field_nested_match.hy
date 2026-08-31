// Nested `match` on the same Option field copies the field; outer
// pattern bindings stay in scope in the inner arm.
class BoxInt {
    pub opt: Option<int>,
}

class Node {
    pub val: int,
    pub left: Option<Node>,
    pub right: Option<Node>,
}

class Holder {
    pub text: Option<string>,
}

fn nested_same_field(BoxInt b) -> int {
    return match b.opt {
        Option::Some(v) => match b.opt {
            Option::Some(v2) => v + v2,
            Option::None => -1,
        },
        Option::None => 0,
    };
}

fn sequential_same_field(BoxInt b) -> int {
    let first = match b.opt {
        Option::Some(v) => v,
        Option::None => -1,
    };
    let second = match b.opt {
        Option::Some(v) => v,
        Option::None => -2,
    };
    return first * 100 + second;
}

fn nested_niche_child(Node n) -> int {
    return match n.left {
        Option::Some(child) => match n.left {
            Option::Some(child2) => child.val + child2.val,
            Option::None => -1,
        },
        Option::None => 0,
    };
}

fn nested_shadows_inner_name(BoxInt b) -> int {
    return match b.opt {
        Option::Some(v) => match Option::Some(100) {
            Option::Some(v) => v,
            Option::None => -1,
        },
        Option::None => 0,
    };
}

fn nested_none_arm_uses_outer(BoxInt b) -> int {
    return match b.opt {
        Option::Some(v) => match Option::None {
            Option::Some(_) => -1,
            Option::None => v,
        },
        Option::None => 0,
    };
}

// After the nested match restores `match_bindings`, the outer name must
// still resolve (not E0100).
fn outer_binding_after_nested_match(BoxInt b) -> int {
    return match b.opt {
        Option::Some(v) => {
            let inner = match Option::Some(1) {
                Option::Some(x) => x,
                Option::None => 0,
            };
            v + inner
        },
        Option::None => 0,
    };
}

fn triple_nested_keeps_outermost(BoxInt box) -> int {
    return match box.opt {
        Option::Some(a) => match box.opt {
            Option::Some(b) => match box.opt {
                Option::Some(c) => a + b + c,
                Option::None => -1,
            },
            Option::None => -2,
        },
        Option::None => 0,
    };
}

fn nested_result_rematch(Result<int, string> r) -> int {
    return match r {
        Result::Ok(v) => match r {
            Result::Ok(v2) => v + v2,
            Result::Err(_) => -1,
        },
        Result::Err(_) => 0,
    };
}

enum Choice {
    A(int),
    B,
}

fn nested_user_enum_rematch(Choice c) -> int {
    return match c {
        Choice::A(x) => match c {
            Choice::A(y) => x + y,
            Choice::B => -1,
        },
        Choice::B => 0,
    };
}

test("nested match on boxed Option field") {
    let b = new BoxInt(Option::Some(21));
    assert(nested_same_field(b) == 42)?;
}

test("matching a field does not consume it") {
    let b = new BoxInt(Option::Some(21));
    assert(sequential_same_field(b) == 2121)?;
}

test("nested match on niche Option class field") {
    let leaf = new Node(3, Option::None, Option::None);
    let root = new Node(1, Option::Some(leaf), Option::None);
    assert(nested_niche_child(root) == 6)?;
}

test("inner binding shadows outer match name") {
    let b = new BoxInt(Option::Some(21));
    assert(nested_shadows_inner_name(b) == 100)?;
}

test("inner None arm still sees outer binding") {
    let b = new BoxInt(Option::Some(21));
    assert(nested_none_arm_uses_outer(b) == 21)?;
}

test("nested match on niche Option string field") {
    let h = new Holder(Option::Some("ok"));
    let n = match h.text {
        Option::Some(s) => match h.text {
            Option::Some(_) => s,
            Option::None => "gone",
        },
        Option::None => "none",
    };
    assert(n == "ok")?;
}

test("outer binding still resolves after nested match") {
    let b = new BoxInt(Option::Some(21));
    assert(outer_binding_after_nested_match(b) == 22)?;
}

test("triple nested match keeps outermost binding") {
    let b = new BoxInt(Option::Some(7));
    assert(triple_nested_keeps_outermost(b) == 21)?;
}

test("nested rematch on Result keeps outer Ok binding") {
    assert(nested_result_rematch(Result::Ok(21)) == 42)?;
}

test("nested rematch on user enum keeps outer binding") {
    assert(nested_user_enum_rematch(Choice::A(21)) == 42)?;
}
