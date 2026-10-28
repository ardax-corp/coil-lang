// String `+=`, bytes round trip, indexing, ordering and every `format` specifier including `%v` on a tuple and a record.
// @output hello world|11|true|h|104|ab,cd|3.5|x=1 y=2.0 z=true s=hi v=Some(1)|%|(1, a)|{ a: 1 }\n
use io::sync::println;
use string::{format, to_bytes, from_bytes};

fn main() {
    let s = "hello";
    s += " world";
    let b = to_bytes(s);
    let back = from_bytes(b).unwrap_or("bad");
    let first = s[0];
    let cmp = "abc" < "abd";
    let _ = println(
        format(
            "%s|%i|%z|%s|%i|%s|%f|x=%i y=%f z=%z s=%s v=%v|%%|%v|%v",
            back,
            s.len(),
            cmp,
            "h",
            first,
            "ab" + "," + "cd",
            3.5,
            1,
            2.0,
            true,
            "hi",
            Option::Some(1),
            (1, "a"),
            { a: 1 },
        ),
    );
}
