// `env::set_var`, `env::var` and `env::remove_var` round-trip a value.
// Output: coil_ok
use env::{remove_var, set_var, var};
use io::{stdout, write};
use string::{format, to_bytes};

fn round_trip() -> string {
    let set_ok = match set_var("COIL_PIPELINE_ENV_KEY", "coil_ok") {
        Result::Ok(_) => 1,
        Result::Err(_) => 0,
    };
    if set_ok == 0 {
        return "0";
    }
    let got = match var("COIL_PIPELINE_ENV_KEY") {
        Result::Ok(s) => s,
        Result::Err(_) => "0",
    };
    let rem_ok = match remove_var("COIL_PIPELINE_ENV_KEY") {
        Result::Ok(_) => 1,
        Result::Err(_) => 0,
    };
    if rem_ok == 0 {
        return "0";
    }
    return got;
}

fn main() {
    let _ = write(stdout(), to_bytes(format("%s", round_trip())));
}
