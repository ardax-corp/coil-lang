// A bounded `task::channel` keeps values in FIFO order, holds at most its
// capacity (a producer waits for the consumer), and wraps around its buffer.
use tests::assert;
use task::{scope, Scope, TaskError, Channel, ChannelError};

fn take(Channel<int> ch) -> int {
    return match ch.recv() {
        Result::Ok(v) => v,
        Result::Err(_) => -1,
    };
}

fn put(Channel<int> ch, int v) {
    match ch.send(v) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "send on a closed channel",
    }
}

test("values come out in the order they went in") {
    let ch: Channel<int> = task::channel(8);
    for i in 0..5 {
        put(ch, i * 3);
    }
    let i = 0;
    while i < 5 {
        assert!(take(ch) == i * 3);
        i += 1;
    }
    assert!(ch.len() == 0);
}

test("the buffer wraps around past its capacity") {
    let ch: Channel<int> = task::channel(3);
    let next = 0;
    let expect = 0;
    let round = 0;
    while round < 10 {
        put(ch, next);
        next += 1;
        put(ch, next);
        next += 1;
        assert!(ch.len() == 2);
        assert!(take(ch) == expect);
        expect += 1;
        assert!(take(ch) == expect);
        expect += 1;
        round += 1;
    }
    assert!(ch.len() == 0 && ch.capacity() == 3);
}

test("a producer never runs more than capacity ahead of the consumer") {
    let ch: Channel<int> = task::channel(2);
    let max_ahead: Vec<int> = Vec::new();
    max_ahead.push(0);
    let r = scope(fn (Scope s) use (ch, mut max_ahead) {
        s.spawn(fn () use (ch, mut max_ahead) {
            for i in 1..21 {
                put(ch, i);
                if ch.len() > max_ahead[0] {
                    max_ahead[0] = ch.len();
                }
            }
            ch.close();
            0
        });
        let sum = 0;
        let last = 0;
        let ordered = true;
        let done = false;
        while !done {
            match ch.recv() {
                Result::Ok(v) => {
                    if v != last + 1 {
                        ordered = false;
                    }
                    last = v;
                    sum += v;
                    task::yield_now();
                },
                Result::Err(_) => {
                    done = true;
                },
            }
        }
        if !ordered {
            sum = -1;
        }
        sum
    });
    assert!(r == Result::Ok(210));
    assert!(max_ahead[0] <= 2);
    assert!(max_ahead[0] >= 1);
}

test("several producers share one channel") {
    let ch: Channel<int> = task::channel(1);
    let r = scope(fn (Scope s) use (ch) {
        let p = 0;
        while p < 3 {
            let base = p * 100;
            s.spawn(fn () use (ch, base) {
                for i in 0..4 {
                    put(ch, base + i);
                }
                0
            });
            p += 1;
        }
        let sum = 0;
        let n = 0;
        while n < 12 {
            sum += take(ch);
            n += 1;
        }
        sum
    });
    // 4 * (0 + 100 + 200) + 3 * (0 + 1 + 2 + 3)
    assert!(r == Result::Ok(1218));
}

test("try_send fills to capacity exactly") {
    let ch: Channel<int> = task::channel(3);
    let accepted = 0;
    let i = 0;
    while i < 5 {
        match ch.try_send(i) {
            Result::Ok(_) => {
                accepted += 1;
            },
            Result::Err(ChannelError::Full) => {},
            Result::Err(_) => {
                accepted = -100;
            },
        }
        i += 1;
    }
    assert!(accepted == 3);
    assert!(ch.len() == 3);
    assert!(take(ch) == 0);
    let room = match ch.try_send(9) {
        Result::Ok(_) => true,
        Result::Err(_) => false,
    };
    assert!(room);
}

test("channel values can be strings and records") {
    let ch: Channel<(string, int)> = task::channel(2);
    let _ = ch.send(("a", 1));
    let _ = ch.send(("b", 2));
    let got = "";
    let total = 0;
    for _ in 0..2 {
        match ch.recv() {
            Result::Ok(pair) => {
                got = got + pair[0];
                total += pair[1];
            },
            Result::Err(_) => {},
        }
    }
    assert!(got == "ab" && total == 3);
}
