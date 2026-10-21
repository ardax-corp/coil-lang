// Expected: E0200 — `Option` is the built-in prelude enum; rebinding the
// prelude name with `use prelude::Option as …` does not free it for a user
// enum (#1672).
use prelude::Option as PreludeOption;

enum Option {
    Yes,
    No,
}

fn yes(Option o) -> bool {
    return match o {
        Option::Yes => true,
        Option::No => false,
    };
}

fn main() {
    let _ = yes(Option::Yes);
}
