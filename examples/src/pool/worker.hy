use thread::{ThreadError, recv};
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn run_jobs(Receiver rx) -> Result<int, ThreadError> {
    while true {
        let job = recv(rx)?;
        if job == "stop" {
            break;
        }
        write_all(stdout(), to_bytes(format("%s,", job)));
    }
    return 0;
}
