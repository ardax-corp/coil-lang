// Unit test: adding tasks grows the board.
use tests::assert;
use board::{add_task, board_len, count_done, empty_board};

test("empty board has length 0") {
    let b = empty_board();
    assert!(board_len(b) == 0, "empty");
}

test("board grows on add") {
    let b = empty_board();
    b = add_task(b, "alpha");
    b = add_task(b, "beta");
    assert!(board_len(b) == 2, "two tasks");
    assert!(count_done(b) == 0, "none done yet");
}
