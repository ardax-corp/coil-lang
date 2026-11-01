// 01-todo — small showcase: classes, an enum field, Vec, Result/?, modules.
//
// Expected output:
//   board: 3 tasks, 1 done
//   1: write tests [Doing]
//   2: ship demo [Todo]
//   3: nap [Done]

use board::{Task, add_task, advance_task, count_done, status_name};

use io::sync::println;
use string::format;

fn main() -> Result<(), string> {
    let board: Vec<Task> = [];
    add_task(board, "write tests");
    add_task(board, "ship demo");
    let nap = add_task(board, "nap");

    advance_task(board, 1)?;
    advance_task(board, nap)?;
    advance_task(board, nap)?;

    let _ = println(format("board: %i tasks, %i done", board.len(), count_done(board)));
    for t in board {
        let _ = println(format("%i: %s [%s]", t.id, t.title, status_name(t.status)));
    }
    return Ok(());
}
