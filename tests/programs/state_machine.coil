// An enum state machine stepped over a `Vec` of events with nested `match`, derived `Eq` and a trace built with `%v`.
// @output State::Idle -> State::Running(3) -> State::Running(2) -> State::Running(1) -> State::Done | ticks=4 | errors=1\n
use io::sync::println;
use string::format;

#[derive(Show, Eq)]
enum State {
    Idle,
    Running(int),
    Done,
    Failed { code: int },
}

#[derive(Show, Eq)]
enum Event {
    Start(int),
    Tick,
    Fail(int),
}

fn step(State s, Event e) -> State {
    return match s {
        State::Idle => match e {
            Event::Start(n) => State::Running(n),
            default => s,
        },
        State::Running(n) => match e {
            Event::Tick => if n <= 1 {
                State::Done
            } else {
                State::Running(n - 1)
            },
            Event::Fail(c) => State::Failed { code: c },
            default => s,
        },
        default => s,
    };
}

fn main() {
    let events = Vec::from([Event::Start(3), Event::Tick, Event::Tick, Event::Tick, Event::Tick]);
    let s = State::Idle;
    let trace = format("%v", s);
    let ticks = 0;
    for e in events {
        let n = step(s, e);
        if e == Event::Tick {
            ticks += 1;
        }
        if n != s {
            trace += format(" -> %v", n);
        }
        s = n;
    }
    let f = step(State::Running(2), Event::Fail(7));
    let errors = match f {
        State::Failed { code } => if code == 7 {
            1
        } else {
            0
        },
        default => 0,
    };
    let _ = println(format("%s | ticks=%i | errors=%i", trace, ticks, errors));
}
