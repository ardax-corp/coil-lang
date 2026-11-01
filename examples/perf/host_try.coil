// `?` on a host call that returns a boxed Result: the result goes
// straight from HostInvoke to JumpIfMatch, with no STORE/LOAD of a
// slot nothing else reads.
use io::{stdout, write};
use string::to_bytes;

fn main() {
    write(stdout(), to_bytes("host try\n"))?;
}
