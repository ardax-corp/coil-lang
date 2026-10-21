// Output: ok,none
use io::{stdout, write};
use string::{format, to_bytes};

fn show(Option<string> value) -> string {
    return match value {
        Option::Some(text) => text,
        Option::None => "none",
    };
}

fn main() {
    let _ = write(stdout(), to_bytes(format("%s,%s", show(Option::Some("ok")), show(Option::None))));
}
