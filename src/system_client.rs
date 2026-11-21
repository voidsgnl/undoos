//! Client for the narrowly scoped privileged system service.
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    time::Duration,
};

const SOCKET_PATH: &str = "/run/lifeline/control.sock";
const STATUS_RESPONSE: &str = "OK system-service=ready protocol=1\n";

/// Ask the system service for its health status.
pub fn status() -> Result<(), String> {
    let mut stream = UnixStream::connect(SOCKET_PATH).map_err(|error| {
        format!(
            "cannot contact the UndoOS system service at {SOCKET_PATH}: {error}.              Check: systemctl status lifeline-system.socket"
        )
    })?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| format!("cannot set service response timeout: {error}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| format!("cannot set service request timeout: {error}"))?;

    stream
        .write_all(b"STATUS\n")
        .map_err(|error| format!("cannot send status request: {error}"))?;

    let mut response = String::new();
    let bytes = BufReader::new(stream)
        .take(128)
        .read_line(&mut response)
        .map_err(|error| format!("cannot read system-service response: {error}"))?;
    if bytes == 0 {
        return Err("system service closed the connection without a response".to_string());
    }
    if response != STATUS_RESPONSE {
        return Err(format!(
            "system service returned an unexpected response: {}",
            response.trim_end()
        ));
    }

    println!("System service: running");
    println!("Control protocol: version 1");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_response_is_a_single_versioned_line() {
        assert_eq!(STATUS_RESPONSE, "OK system-service=ready protocol=1\n");
        assert!(STATUS_RESPONSE.ends_with('\n'));
    }
}
