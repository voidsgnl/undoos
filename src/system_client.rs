//! Client for the narrowly scoped privileged system service.
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    time::Duration,
};

const SOCKET_PATH: &str = "/run/lifeline/control.sock";
const STATUS_RESPONSE: &str = "OK system-service=ready protocol=1\n";

/// Ask the system service for its health status.
pub fn status() -> Result<(), String> {
    let mut stream = UnixStream::connect(SOCKET_PATH).map_err(|error| {
        format!(
            "cannot contact the UndoOS system service at {SOCKET_PATH}: {error}. Check: systemctl status lifeline-system.socket"
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

fn parse_capture_response(response: &str) -> Result<(String, u64), String> {
    if response.trim_end() == "ERROR capture-failed" {
        return Err(
            "system-file capture was denied or failed; inspect the service log with: sudo journalctl -u lifeline-system.service -n 30 --no-pager"
                .to_string(),
        );
    }
    let fields = response.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 5
        || fields[0] != "OK"
        || fields[1] != "captured"
        || !fields[2].starts_with("object=")
        || !fields[3].starts_with("size=")
        || fields[4] != "path=/etc/hostname"
    {
        return Err(format!(
            "system service returned an unexpected capture response: {}",
            response.trim_end()
        ));
    }
    let object_id = &fields[2]["object=".len()..];
    if object_id.len() != 32
        || !object_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("system service returned an invalid recovery object ID".to_string());
    }
    let size = fields[3]["size=".len()..]
        .parse::<u64>()
        .map_err(|_| "system service returned an invalid captured size".to_string())?;
    Ok((object_id.to_string(), size))
}

/// Ask the privileged service to capture its single initially approved file.
///
/// The file path is intentionally not supplied by the client: the service
/// selects and validates /etc/hostname against its own exact-path policy.
pub fn capture_hostname() -> Result<(), String> {
    let mut stream = UnixStream::connect(SOCKET_PATH).map_err(|error| {
        format!(
            "cannot contact the UndoOS system service at {SOCKET_PATH}: {error}. Check: systemctl status lifeline-system.socket"
        )
    })?;
    stream
        .set_read_timeout(Some(Duration::from_secs(120)))
        .map_err(|error| format!("cannot set capture response timeout: {error}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| format!("cannot set capture request timeout: {error}"))?;
    stream
        .write_all(b"CAPTURE-HOSTNAME\n")
        .map_err(|error| format!("cannot send system capture request: {error}"))?;

    let mut response = String::new();
    let bytes = BufReader::new(stream)
        .take(512)
        .read_line(&mut response)
        .map_err(|error| format!("cannot read system capture response: {error}"))?;
    if bytes == 0 {
        return Err("system service closed the connection without a capture response".to_string());
    }
    let (object_id, size) = parse_capture_response(&response)?;

    println!("Captured /etc/hostname: {size} bytes");
    println!("Recovery object: {object_id}");
    Ok(())
}

#[cfg(test)]
mod capture_response_tests {
    use super::*;

    #[test]
    fn parses_a_valid_fixed_path_capture_response() {
        let response =
            "OK captured object=0123456789abcdef0123456789abcdef size=12 path=/etc/hostname\n";
        let parsed = parse_capture_response(response).unwrap();
        assert_eq!(parsed.0, "0123456789abcdef0123456789abcdef");
        assert_eq!(parsed.1, 12);
    }

    #[test]
    fn rejects_a_capture_response_for_an_unapproved_path() {
        let response =
            "OK captured object=0123456789abcdef0123456789abcdef size=12 path=/etc/passwd\n";
        assert!(parse_capture_response(response).is_err());
    }

    #[test]
    fn rejects_an_invalid_recovery_object_id() {
        let response = "OK captured object=not-an-id size=12 path=/etc/hostname\n";
        assert!(parse_capture_response(response)
            .unwrap_err()
            .contains("invalid recovery object ID"));
    }

    #[test]
    fn reports_a_bounded_capture_failure_response() {
        assert!(parse_capture_response("ERROR capture-failed\n")
            .unwrap_err()
            .contains("sudo journalctl"));
    }
}
