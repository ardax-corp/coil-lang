// Todo board logic, imported by main and by the tests via `use board::{…}`.

#[derive(Eq)]
enum Status {
    Todo,
    Doing,
    Done,
}

class Task {
    pub id: int,
    pub title: string,
    pub status: Status,
}

fn status_name(Status s) -> string {
    return match s {
        Status::Todo => "Todo",
        Status::Doing => "Doing",
        Status::Done => "Done",
    };
}

/// Adds a task in `Todo` and returns its id (ids start at 1).
fn add_task(Vec<Task> board, string title) -> int {
    let id = board.len() + 1;
    board.push(new Task(id, title, Status::Todo));
    return id;
}

/// Moves a task one step: `Todo` to `Doing` to `Done`.
fn advance_task(Vec<Task> board, int id) -> Result<Task, string> {
    if id < 1 || id > board.len() {
        raise "unknown task id";
    }
    let t = mut board[id - 1];
    t.status = match t.status {
        Status::Todo => Status::Doing,
        default => Status::Done,
    };
    return t;
}

fn count_done(Vec<Task> board) -> int {
    let n = 0;
    for t in board {
        if t.status == Status::Done {
            n = n + 1;
        }
    }
    return n;
}
