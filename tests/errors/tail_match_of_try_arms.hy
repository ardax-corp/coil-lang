// @output ok\nnone\nmain ok
// A trailing `match` whose arms end in `x?` is the body's last statement,
// not its value: the `int` the arms give is dropped and the function falls
// off into `Ok(())` (#1844). In `main` and in any unit function that uses `?`.
use io::sync::println;

fn run() -> Result<int, string> {
    return Ok(1);
}

fn opt(int n) -> Option<int> {
    if n > 0 {
        return Some(n);
    }
    return None;
}

fn report() {
    match run() {
        Ok(n) => println("ok")?,
        Err(e) => println(e)?,
    }
}

fn report_opt() {
    match opt(0) {
        Some(n) => println("some")?,
        None => println("none")?,
    }
}

fn main() {
    report()?;
    report_opt()?;
    match run() {
        Ok(n) => println("main ok")?,
        Err(e) => println(e)?,
    }
}
