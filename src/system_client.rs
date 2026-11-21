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

#[derive(Debug, PartialEq, Eq)]
enum HostnameHistoryEvent {
    Captured { object_id: String, timestamp: u128, size: u64 },
    Deleted { previous_object_id: String, timestamp: u128 },
}

fn parse_hostname_history(response: &str) -> Result<Vec<HostnameHistoryEvent>, String> {
    let mut lines = response.lines();
    let header = lines.next().ok_or_else(|| "system service returned an empty history response".to_string())?;
    let count = header.strip_prefix("OK history count=").ok_or_else(|| format!("unexpected system history response: {header}"))?
        .parse::<usize>().map_err(|_| "system service returned an invalid history count".to_string())?;
    if count > 20 { return Err("system service returned more than the 20-entry history limit".to_string()); }
    let mut entries = Vec::with_capacity(count);
    for line in lines {
        let fields = line.split('|').collect::<Vec<_>>();
        if fields.len() != 4 || fields[1].len() != 32
            || !fields[1].bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) {
            return Err("system service returned an invalid history entry".to_string());
        }
        let timestamp = fields[2].parse::<u128>().map_err(|_| "system service returned an invalid event timestamp".to_string())?;
        let size = fields[3].parse::<u64>().map_err(|_| "system service returned an invalid event size".to_string())?;
        match fields[0] {
            "C" => entries.push(HostnameHistoryEvent::Captured { object_id: fields[1].to_string(), timestamp, size }),
            "D" if size == 0 => entries.push(HostnameHistoryEvent::Deleted { previous_object_id: fields[1].to_string(), timestamp }),
            _ => return Err("system service returned an unknown history event type".to_string()),
        }
    }
    if entries.len() != count { return Err("system service history count does not match its entries".to_string()); }
    Ok(entries)
}

/// List the most recent captured versions of the approved hostname file.
pub fn hostname_history() -> Result<(), String> {
    let mut stream = connect_system_service()?;
    stream
        .write_all(b"HISTORY-HOSTNAME\n")
        .map_err(|error| format!("cannot request system history: {error}"))?;
    let mut response = String::new();
    BufReader::new(stream)
        .take(8192)
        .read_to_string(&mut response)
        .map_err(|error| format!("cannot read system history: {error}"))?;
    let entries = parse_hostname_history(&response)?;

    println!("System file history: /etc/hostname");
    if entries.is_empty() {
        println!("No captured versions yet.");
        return Ok(());
    }
    for entry in entries {
        match entry {
            HostnameHistoryEvent::Captured { object_id, timestamp, size } =>
                println!("{object_id}  timestamp={timestamp}  size={size} bytes"),
            HostnameHistoryEvent::Deleted { previous_object_id, timestamp } =>
                println!("DELETED  timestamp={timestamp}  previous-object={previous_object_id}"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod hostname_history_tests {
    use super::*;

    #[test]
    fn parses_an_empty_hostname_history() {
        assert!(parse_hostname_history("OK history count=0\n").unwrap().is_empty());
    }

    #[test]
    fn parses_a_hostname_history_entry() {
        let response = "OK history count=1\n0123456789abcdef0123456789abcdef|123456789|7\n";
        let entries = parse_hostname_history(response).unwrap();
        assert_eq!(entries, vec![HostnameHistoryEvent::Captured {
            object_id: "0123456789abcdef0123456789abcdef".to_string(),
            timestamp: 123456789,
            size: 7,
        }]);
    }

    #[test]
    fn parses_a_hostname_deletion_event() {
        let response = "OK history count=1\nD|0123456789abcdef0123456789abcdef|123456789|0\n";
        assert_eq!(parse_hostname_history(response).unwrap(), vec![HostnameHistoryEvent::Deleted {
            previous_object_id: "0123456789abcdef0123456789abcdef".to_string(),
            timestamp: 123456789,
        }]);
    }

    #[test]
    fn rejects_a_history_count_that_does_not_match_the_entries() {
        assert!(parse_hostname_history("OK history count=2\n").is_err());
        assert!(parse_hostname_history("OK history count=0\n0123456789abcdef0123456789abcdef|1|1\n").is_err());
    }

    #[test]
    fn rejects_more_than_twenty_history_entries() {
        assert!(parse_hostname_history("OK history count=21\n").is_err());
    }

    #[test]
    fn rejects_malformed_history_entries() {
        assert!(parse_hostname_history("OK history count=1\nnot-an-id|1|1\n").is_err());
        assert!(parse_hostname_history("OK history count=1\n0123456789abcdef0123456789abcdef|bad|1\n").is_err());
    }
}

/// Restore one previously captured /etc/hostname version.
pub fn restore_hostname(object_id: &str) -> Result<(), String> {
    if object_id.len() != 32
        || !object_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("invalid recovery object ID; expected 32 lowercase hexadecimal characters".to_string());
    }
    let mut stream = connect_system_service()?;
    let request = format!("RESTORE-HOSTNAME {object_id}\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("cannot send system restore request: {error}"))?;
    let mut response = String::new();
    let bytes = BufReader::new(stream)
        .take(512)
        .read_line(&mut response)
        .map_err(|error| format!("cannot read system restore response: {error}"))?;
    if bytes == 0 {
        return Err("system service closed the connection without a restore response".to_string());
    }
    if response.trim_end() == "ERROR restore-failed" {
        return Err(
            "system-file restore failed; inspect the service log with: sudo journalctl -u lifeline-system.service -n 40 --no-pager"
                .to_string(),
        );
    }
    let backup_id = response
        .trim_end()
        .strip_prefix("OK restored backup=")
        .ok_or_else(|| format!("unexpected system restore response: {}", response.trim_end()))?;
    if backup_id.len() != 32
        || !backup_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("system service returned an invalid pre-restore backup ID".to_string());
    }
    println!("Restored /etc/hostname from recovery object: {object_id}");
    println!("Previous hostname saved as recovery object: {backup_id}");
    Ok(())
}

fn connect_system_service() -> Result<UnixStream, String> {
    let stream = UnixStream::connect(SOCKET_PATH).map_err(|error| {
        format!(
            "cannot contact the UndoOS system service at {SOCKET_PATH}: {error}. Check: systemctl status lifeline-system.socket"
        )
    })?;
    stream
        .set_read_timeout(Some(Duration::from_secs(120)))
        .map_err(|error| format!("cannot set system-service response timeout: {error}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| format!("cannot set system-service request timeout: {error}"))?;
    Ok(stream)
}
