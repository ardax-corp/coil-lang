// A trait default method may share its name with a free fn of another
// result mode: `pieces` below returns a plain `Vec`, the free `pieces` a
// `Result`. The default body compiles in its own mode.
use tests::assert;

fn pieces(string s) -> Result<Vec<string>, string> {
    if s.len() == 0 {
        raise "empty";
    }
    let out: Vec<string> = Vec::new();
    out.push(s);
    return out;
}

trait Split<T: Show> {
    fn pieces(T value) -> Vec<string> {
        let out: Vec<string> = Vec::new();
        out.push(value.show());
        out.push(value.show());
        return out;
    }
}

impl Split for int {
}

test("a default method and a free fn share a name") {
    assert!(Split::pieces(7).len() == 2);
    assert!(7.pieces()[1] == "7");
    let free = pieces("a") ?? Vec::new();
    assert!(free.len() == 1);
    assert!(pieces("").is_err());
}
