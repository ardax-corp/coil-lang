use io::{stdout, write};

use string::{format, to_bytes};

class TextCounter {
    pub cur: int,
    pub end: int,
    pub text: string,
}

impl IntoIterator for TextCounter {
    type Item = string;
    type IntoIter = TextCounter;
    fn into_iter(TextCounter value) -> TextCounter {
        return value;
    }
}

impl Iterator for TextCounter {
    type Item = string;
    fn next(TextCounter value) -> Option<string> {
        if value.cur < value.end {
            value.cur = value.cur + 1;
            return Option::Some(value.text,);
        }
        return Option::None;
    }
}

fn main() {
    let value = new TextCounter(0, 2, "x");
    for text in value {
        write(stdout(), to_bytes(format("%s", text)));
    }
}
