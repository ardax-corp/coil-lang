// Server-side pure helpers for the echo demo.
// TCP accept/read/write is in `main.hy` (the server task).

/// Echo policy: reply with the same framed bytes that arrived.
fn echo_reply(Vec<byte> frame) -> Vec<byte> {
    return frame;
}
