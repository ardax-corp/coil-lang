// COI-16: inlined `Vec::push` must stage the receiver when the arg emits
// `STORE`/`Seek` (`format`, `match`, `new Class`). Pre-fix, those pushes
// silently dropped values (enum serialize via match+format yielded only the
// header; `v.push(new C(...))` left len=0).
// format / new Class / match+format push args must not drop the vec
// Output: lines=2 pkgs=2 names=1 n0=name='n'
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

enum LockPackage {
    Git(string, string, string),
}

class Point {
    pub x: int,
    pub y: int,
}

fn quote(string s) -> string {
    return "'" + s + "'";
}

fn main() {
    let lines = Vec::new();
    lines.push(format("a=%s", "1"));
    lines.push(format("b=%s", "2"));
    let pkgs = Vec::new();
    pkgs.push(new Point(1, 2));
    pkgs.push(new Point(3, 4));
    let enums = Vec::new();
    enums.push(LockPackage::Git("n", "g", "t"));
    let e0 = enums[0];
    let names = Vec::new();
    names
        .push(
            match e0 {
                LockPackage::Git(name, git, tag) => format("name=%s", quote(name)),
            },
        );
    let _ = write_all(
        stdout(),
        to_bytes(
            format("lines=%i pkgs=%i names=%i n0=%s", len(lines), len(pkgs), len(names), names[0]),
        ),
    );
}
