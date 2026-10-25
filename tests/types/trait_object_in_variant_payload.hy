// @output rex\nnone\nfido\n
// A class value built into a variant whose payload is a trait object
// packs as that trait, as it does at a `let` of the trait (#2002).
use io::sync::println;

trait Named<T> {
    fn name(T x) -> string;
}

class Dog {
    pub n: string,
}

impl Named for Dog {
    fn name(Dog d) -> string {
        return d.n;
    }
}

fn show(Option<Named> o) -> string {
    return match o {
        Option::Some(x) => name(x),
        Option::None => "none",
    };
}

fn found(bool ok) -> Result<Named, string> {
    if ok {
        return Result::Ok(new Dog("fido"));
    }
    return Result::Err("missing");
}

fn main() {
    let some: Option<Named> = Option::Some(new Dog("rex"));
    let _ = println(show(some));
    let _ = println(show(Option::None));
    match found(true) {
        Result::Ok(d) => {
            let _ = println(name(d));
        },
        Result::Err(e) => {
            let _ = println(e);
        },
    }
}
