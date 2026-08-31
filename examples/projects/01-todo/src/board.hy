// Todo board logic — imported by main and by tests via `use board::{…}`.
//
// Status is an int (0=Todo, 1=Doing, 2=Done): comparing an enum field on a
// class instance (`t.status == Status::Todo`) has crashed the VM in the past.
// Index 0 is a sentinel Task — real tasks live at indices 1..len-1.

class Task {
    pub id: int,
    pub title: string,
    pub status: int,
}

fn empty_board() -> Vec<Task> {
    let seed = new Task(0, "", 0);
    let board = Vec::from([seed]);
    return board;
}

fn add_task(Vec<Task> board, string title) {
    let id = len(board);
    let t = new Task(id, title, 0);
    board.push(t);
    return board;
}

fn advance_task(Vec<Task> board, int id) -> Task {
    let n = len(board) - 1;
    if id < 1 {
        panic "bad id";
    }
    if id > n {
        panic "unknown task id";
    }
    let t = board[id];
    if t.status == 0 {
        t.status = 1;
    } else {
        if t.status == 1 {
            t.status = 2;
        }
    }
    return t;
}

fn count_done(Vec<Task> board) -> int {
    let n = 0;
    let i = 1;
    while i < len(board) {
        let t = board[i];
        if t.status == 2 {
            n = n + 1;
        }
        i = i + 1;
    }
    return n;
}

fn board_len(Vec<Task> board) -> int {
    return len(board) - 1;
}

fn status_name(int s) -> string {
    if s == 0 {
        return "Todo";
    }
    if s == 1 {
        return "Doing";
    }
    return "Done";
}
