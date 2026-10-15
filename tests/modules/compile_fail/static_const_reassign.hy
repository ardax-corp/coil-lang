// Expected: E0107 — cannot reassign module static const.
static const VERSION = "1.0";

fn main() {
    VERSION = "2.0";
}
