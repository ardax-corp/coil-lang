// A signature too long for one line wraps its parameters one per line and
// keeps `-> Ret {` together on the closing line (#1743). This file is the
// expected `coil fmt` output.
use tests::assert;

class Locks {
    pub n: int,
}

impl Locks {
    pub fn locks_write_all(
        string path,
        Vec<string> packages,
        Vec<string> allow,
    ) -> Result<int, string> {
        return Result::Ok(self.n + path.len() + packages.len() + allow.len());
    }
}

fn lock_serialize_all_entries(
    string path,
    Vec<string> packages,
    Vec<string> allow_include,
    int n,
) -> string {
    return path;
}

fn lock_write_all(
    string path,
    Vec<string> packages,
    Vec<string> allow_include,
    Vec<string> scripts,
) -> Result<int, string> {
    return Result::Ok(path.len() + packages.len() + allow_include.len() + scripts.len());
}

fn lock_count_entries(
    string path,
    Vec<string> packages,
    Vec<string> allow_include,
    Vec<string> sc,
) {
    assert!(path.len() + packages.len() + allow_include.len() + sc.len() > 0);
}

test("wrapped signatures still bind every parameter") {
    let v: Vec<string> = Vec::new();
    v.push("a");
    assert!(lock_serialize_all_entries("p", v, v, 1) == "p");
    assert!(lock_write_all("pq", v, v, v) == Result::Ok(5));
    lock_count_entries("p", v, v, v)?;
    let l = new Locks(1);
    assert!(l.locks_write_all("p", v, v) == Result::Ok(4));
}
