// A bool argument to a generic host word (`set_nodelay`) is a word on
// direct MIR; `nodelay_code` used to refuse ("arg 1 is bool vs i64").
use io::stdout;
use io::net::tcp::set_nodelay;
use tests::assert;

fn nodelay_code(bool on) -> int {
    return match set_nodelay(stdout(), on) {
        Result::Ok(_) => 0,
        Result::Err(_) => 1,
    };
}

test("set_nodelay on a non-socket fails for either flag") {
    assert!(nodelay_code(true) == 1);
    assert!(nodelay_code(false) == 1);
}
