// @output ok one\nerr bad\nerr bad\nok\n
// #1905: a thread returning a Result joins as that Result, Err included.

use io::sync::println;
use thread::{join, spawn};

fn threaded(Result<string, string> x) -> Result<string, string> {
    let th = match spawn(fn () use (x) => x) {
        Ok(t) => t,
        Err(_) => panic "spawn",
    };
    return match join(th) {
        Ok(v) => v,
        Err(_) => panic "join",
    };
}

fn unit(Result<(), string> x) -> Result<(), string> {
    let th = match spawn(fn () use (x) => x) {
        Ok(t) => t,
        Err(_) => panic "spawn",
    };
    return match join(th) {
        Ok(v) => v,
        Err(_) => panic "join",
    };
}

fn main() {
    let a: Result<string, string> = Ok("one");
    let b: Result<string, string> = Err("bad");
    println(match threaded(a) {
        Ok(s) => "ok " + s,
        Err(e) => "err " + e,
    })?;
    println(match threaded(b) {
        Ok(s) => "ok " + s,
        Err(e) => "err " + e,
    })?;
    let c: Result<(), string> = Err("bad");
    println(match unit(c) {
        Ok(_) => "ok",
        Err(e) => "err " + e,
    })?;
    println(match unit(Ok(())) {
        Ok(_) => "ok",
        Err(e) => "err " + e,
    })?;
}
