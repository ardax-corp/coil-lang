// `coil fmt` keeps every line within 100 columns, counting the `;` that
// ends a statement (#1795): a call or `+` chain that is 100 columns wide
// without its `;` is broken, one that is 99 columns wide stays on one line.

use tests::assert;

class Sink {
    pub total: int,
}

impl Sink {
    pub fn push_three(int a, int b, int c) {
        self.total = self.total + a + b + c;
    }
}

fn add_three(int a, int b, int c) -> int {
    return a + b + c;
}

test("a call 100 columns wide before its ; is broken") {
    let first_argument_value = 1;
    let second_argument_value = 2;
    let third_argument_value_xxxxxxx = 3;
    let third_argument_value_xxxxxxxxxxxxx = 3;
    let third_argument_value_xxxxxxxxxxxxxxxxxxx = 3;
    let total = add_three(
        first_argument_value,
        second_argument_value,
        third_argument_value_xxxxxxx,
    );
    let sink = new Sink(0);
    sink
        .push_three(
            first_argument_value,
            second_argument_value,
            third_argument_value_xxxxxxxxxxxxx,
        );
    add_three(
        first_argument_value,
        second_argument_value,
        third_argument_value_xxxxxxxxxxxxxxxxxxx,
    );
    let sum_of_3 = first_argument_value + second_argument_value +
                   third_argument_value_xxxxxxxxxxxxx;
    let fits = add_three(first_argument_value, second_argument_value, third_argument_value_xxxxxxx);
    assert!(total == 6);
    assert!(sink.total == 6);
    assert!(fits == 6);
    assert!(sum_of_3 == 6);
}
