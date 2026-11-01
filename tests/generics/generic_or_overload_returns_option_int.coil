// A generic or overloaded function whose result is a closed two-word value
// (`Option<int>`, `Result<int, string>`) returns it as its pair (#2005).
use tests::assert;

#[derive(Eq)]
enum Color {
    Red,
    Green,
}

fn index_of<T: Eq>(Vec<T> xs, T needle) -> Option<int> {
    let i = 0;
    while i < xs.len() {
        if xs[i] == needle {
            return Option::Some(i);
        }
        i = i + 1;
    }
    return Option::None;
}

fn count_or_err<T>(Vec<T> xs) -> Result<int, string> {
    if xs.len() == 0 {
        raise "empty";
    }
    return xs.len();
}

fn position(Vec<int> xs, int needle) -> Option<int> {
    let i = 0;
    while i < xs.len() {
        if xs[i] == needle {
            return Option::Some(i);
        }
        i = i + 1;
    }
    return Option::None;
}

fn position(Vec<string> xs, string needle) -> Option<int> {
    let i = 0;
    while i < xs.len() {
        if xs[i] == needle {
            return Option::Some(i);
        }
        i = i + 1;
    }
    return Option::None;
}

test("a generic function returns Option<int> at each instance") {
    let ints: Vec<int> = [3, 1, 4];
    let words: Vec<string> = ["a", "b"];
    let floats: Vec<float> = [1.5, 2.5];
    let colors: Vec<Color> = [Color::Red, Color::Green];
    assert!(index_of(ints, 4) == Option::Some(2));
    assert!(index_of(ints, 9).is_none());
    assert!(index_of(words, "b") == Option::Some(1));
    assert!((index_of(floats, 2.5) ?? -1) == 1);
    assert!(index_of(colors, Color::Green) == Option::Some(1));
}

test("a generic function returns Result<int, string>") {
    let ints: Vec<int> = [3, 1];
    let none: Vec<string> = Vec::new();
    assert!(count_or_err(ints) == Result::Ok(2));
    assert!(count_or_err(none).is_err());
}

test("an overloaded function returns Option<int>") {
    let ints: Vec<int> = [3, 1];
    let words: Vec<string> = ["a", "b"];
    assert!(position(ints, 1) == Option::Some(1));
    assert!(position(words, "z").is_none());
    let at = match position(words, "a") {
        Option::Some(i) => i,
        Option::None => -1,
    };
    assert!(at == 0);
}
