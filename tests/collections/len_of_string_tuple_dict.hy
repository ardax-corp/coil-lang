// Structural `len` on non-literal values: string hits VM `ArrayLen`;
// tuple/dict use typed static length after binding (not literal fold).
// Output: 2\n3\n2\n
use io::{stdout, write};
use string::{format, to_bytes};

fn id_str(string s) -> string {
    return s;
}

fn main() {
    let t = (1, 2, 3);
    let d = { a: 1, b: 2 };
    write(stdout(), to_bytes(format("%i\n", len(id_str("ab")))));
    write(stdout(), to_bytes(format("%i\n", len(t))));
    write(stdout(), to_bytes(format("%i\n", len(d))));
}
