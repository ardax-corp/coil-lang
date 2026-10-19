// `.member` takes a line break before it after every receiver (#1540), so
// `coil fmt` can break a method chain on a literal receiver (#1505). The
// chains below are long enough that the formatter puts each `.member` on its
// own line. Spaced forms after a `.field` or a postfix `?` (`x? .y`) are
// covered by the parser unit tests.
use tests::assert;

class Holder {
    pub text: string,
}

fn longest_name_in_this_test_file_to_force_a_break(int n) -> int {
    return n;
}

test("string literal receiver") {
    let shown_length_of_the_alphabet_after_a_few_round_trips = "abcdefghijklmnopqrstuvwxyz"
        .len()
        .show()
        .len()
        .show();
    assert!(shown_length_of_the_alphabet_after_a_few_round_trips == "2");
}

test("number literal receivers") {
    let shown_length_of_an_integer_literal_after_some_round_trips = 1234567
        .show()
        .len()
        .show()
        .len()
        .show();
    assert!(shown_length_of_an_integer_literal_after_some_round_trips == "1");
    let shown_length_of_a_float_literal_after_a_few_round_trips_more = 2.5
        .show()
        .len()
        .show()
        .len()
        .show();
    assert!(shown_length_of_a_float_literal_after_a_few_round_trips_more == "1");
}

test("field and call receivers") {
    let h = new Holder("abcd");
    let length_of_the_field_text_shown_and_measured_again_and_again = h.text.len().show().len();
    assert!(length_of_the_field_text_shown_and_measured_again_and_again == 1);
    assert!(longest_name_in_this_test_file_to_force_a_break(3).show().len() == 1);
}

test("floats and ranges are unchanged") {
    let f = 1.5;
    assert!(f > 1.0);
    let total = 0;
    for i in 0..3 {
        total += i;
    }
    for i in 1..=2 {
        total += i;
    }
    assert!(total == 6);
}
