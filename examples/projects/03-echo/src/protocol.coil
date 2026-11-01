// Length-prefixed frame helpers for the echo demo.
// Frame layout: [nbytes:byte][payload…] (payload capped at 5 for the demo).

fn frame_len(Vec<byte> frame) -> int {
    if frame.len() < 1 {
        return 0;
    }
    let b = frame[0];
    let one: byte = 1;
    let two: byte = 2;
    let three: byte = 3;
    let four: byte = 4;
    let five: byte = 5;
    if b == one {
        return 1;
    }
    if b == two {
        return 2;
    }
    if b == three {
        return 3;
    }
    if b == four {
        return 4;
    }
    if b == five {
        return 5;
    }
    return 0;
}

fn encode_frame(Vec<byte> body) -> Vec<byte> {
    let n = body.len();
    let nb: byte = 0;
    if n == 1 {
        nb = 1;
    }
    if n == 2 {
        nb = 2;
    }
    if n == 3 {
        nb = 3;
    }
    if n == 4 {
        nb = 4;
    }
    if n >= 5 {
        nb = 5;
    }
    let frame: Vec<byte> = Vec::new();
    frame.push(nb);
    let i = 0;
    while i < n {
        if i < 5 {
            frame.push(body[i]);
        }
        i = i + 1;
    }
    return frame;
}

fn payload_eq(Vec<byte> frame, Vec<byte> expect) -> int {
    let n = frame_len(frame);
    if n != expect.len() {
        return 0;
    }
    if frame.len() < n + 1 {
        return 0;
    }
    let i = 0;
    while i < n {
        if frame[i + 1] != expect[i] {
            return 0;
        }
        i = i + 1;
    }
    return 1;
}
