// @expected E0406 — a gated call inside a method `main` calls is reached.
use env::exec;

class Runner {
    pub program: string,
}

impl Runner {
    pub fn go() -> int {
        let args: Vec<string> = Vec::new();
        return match exec(self.program, args) {
            Result::Ok(code) => code,
            Result::Err(_) => -1,
        };
    }
}

fn main() {
    let r = new Runner("true");
    let _ = r.go();
}
