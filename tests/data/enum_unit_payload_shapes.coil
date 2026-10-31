// More shapes of a `()` enum payload: a class field, a `Vec` element,
// nesting, `?`, `==` and a bound payload passed on (#1449).
use tests::assert;
use string::format;

class Holder {
    pub o: Option<()>,
}

fn show_unit(unit u) -> string {
    return format("%v", u);
}

fn first(Vec<Option<()>> v) -> bool {
    return match v[0] {
        Some(_) => true,
        None => false,
    };
}

fn nested(Option<Option<()>> o) -> int {
    return match o {
        Some(Some(_)) => 2,
        Some(None) => 1,
        None => 0,
    };
}

fn ask(bool b) -> Option<()> {
    if b {
        return Some(());
    }
    return None;
}

fn chain(bool b) -> Option<int> {
    ask(b)?;
    return Some(7);
}

fn bind_show(Option<()> o) -> string {
    return match o {
        Some(u) => show_unit(u),
        None => "none",
    };
}

fn both(int n) -> Result<(), ()> {
    if n > 0 {
        return Result::Ok(());
    }
    return Result::Err(());
}

test("unit payloads in fields, vecs, nesting and ?") {
    assert!(match both(1) {
        Result::Ok(_) => true,
        Result::Err(_) => false,
    });
    assert!(
        match both(0) {
            Result::Ok(_) => false,
            Result::Err(e) => show_unit(e) == "()",
        },
    );
    assert!(bind_show(Some(())) == "()");
    assert!(bind_show(None) == "none");
    let h = new Holder(Some(()));
    assert!(match h.o {
        Some(_) => true,
        None => false,
    });
    let v: Vec<Option<()>> = [Some(()), None];
    assert!(first(v));
    assert!(nested(Some(Some(()))) == 2);
    assert!(nested(Some(None)) == 1);
    assert!(nested(None) == 0);
    assert!(chain(true) == Some(7));
    assert!(chain(false) == None);
    assert!(ask(true) == Some(()));
    assert!(ask(false) != Some(()));
}
