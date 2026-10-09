// Client-side pure helpers for the echo demo.
// TCP connect/send/recv is in `main.hy` (the client task).

fn request_body() -> Vec<byte> {
    let a: byte = 65;
    let b: byte = 66;
    let body: Vec<byte> = Vec::new();
    body.push(a);
    body.push(b);
    return body;
}
