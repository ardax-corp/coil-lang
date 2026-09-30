// Expected: compile failure — bitwise operators take `int` / `byte`;
// `&&` / `||` are the boolean connectives (#554).
fn main() {
    let a = true & false;
    let b = true | false;
    let c = 1.5 << 2;
}
