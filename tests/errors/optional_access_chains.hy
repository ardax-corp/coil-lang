// `a?.f` on `Option<R>` is `Option<U>`: `None` stays `None`, `Some(x)`
// becomes `Some(x.f)`. Chains go through nested objects and end in `??`.
use tests::assert;

class Inner {
    pub v: int,
}

class Person {
    pub name: string,
    pub age: int,
    pub inner: Inner,
}

fn lookup(int id) -> Option<Person> {
    if id == 1 {
        return Option::Some(new Person("ada", 36, new Inner(7)));
    }
    return Option::None;
}

test("Some maps to the field, None stays None") {
    assert!(lookup(1)?.age == Option::Some(36));
    assert!(lookup(2)?.age == Option::None);
    let some = Option::Some({ v: 42 });
    assert!(some?.v == Option::Some(42));
}

test("ending a chain with ??") {
    assert!((lookup(1)?.name ?? "?") == "ada");
    assert!((lookup(2)?.name ?? "?") == "?");
    assert!((lookup(1)?.age ?? 0) == 36);
    assert!((lookup(2)?.age ?? -1) == -1);
}

test("through a nested object") {
    assert!((lookup(1)?.inner?.v ?? 0) == 7);
    assert!((lookup(2)?.inner?.v ?? 0) == 0);
}
