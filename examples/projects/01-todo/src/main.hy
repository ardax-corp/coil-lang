// 01-todo — small showcase: classes, arrays, Result/?, format, modules.
//
// Expected output:
//   board:3 done:1 | 1:write tests [Doing] | 2:ship demo [Todo] | 3:nap [Done] |

use board::{Task, add_task, advance_task, board_len, count_done, empty_board, status_name};

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn print_task(Task t) {
    write_all(stdout(), to_bytes(format("%i:%s [%s] | ", t.id, t.title, status_name(t.status))));
}

fn main() {
    let board = empty_board();
    board = add_task(board, "write tests");
    board = add_task(board, "ship demo");
    board = add_task(board, "nap");

    advance_task(board, 1);
    advance_task(board, 3);
    advance_task(board, 3);

    write_all(
        stdout(),
        to_bytes(format("board:%i done:%i | ", board_len(board), count_done(board))),
    );

    let i = 1;
    while i <= board_len(board) {
        print_task(board[i]);
        i = i + 1;
    }
}
