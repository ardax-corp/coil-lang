// #915: a generic fn whose body calls another generic fn on its own type
// parameter.
fn id<T>(T x) -> T {
    return x;
}

fn id2<T>(T x) -> T {
    return id(x);
}

fn wrap<T>(T x) -> Option<T> {
    return Option::Some(x);
}

fn twice<T>(T x) -> Option<Option<T>> {
    return Option::Some(wrap(x));
}

test("a generic fn calls another generic fn") {
    assert(id2(2.5) == 2.5)?;
    assert(id2(7) == 7)?;
    assert(id2("s") == "s")?;
    let n = match twice(3) {
        Option::Some(inner) => match inner {
            Option::Some(v) => v,
            Option::None => -1,
        },
        Option::None => -2,
    };
    assert(n == 3)?;
}
