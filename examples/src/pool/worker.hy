use thread::{ThreadError, recv};
use io::sync::print;
use string::format;

fn run_jobs(Receiver rx) -> Result<int, ThreadError> {
    while true {
        let job = recv(rx)?;
        if job == "stop" {
            break;
        }
        let _ = print(format("%s,", job));
    }
    return 0;
}
