//! Minimal root-owned system service for the future privileged recovery boundary.
//!
//! This first version deliberately supports only a root-authenticated PING/PONG
//! health check. It does not inspect files or perform privileged mutations.

use std::{
    env,
    io::{Read, Write},
    os::fd::FromRawFd,
    os::unix::net::{UnixListener, UnixStream},
    process,
    thread,
    time::Duration,
};

const SYSTEMD_LISTEN_FDS: &str = "LISTEN_FDS";
const SYSTEMD_LISTEN_PID: &str = "LISTEN_PID";
const MAX_REQUEST_BYTES: usize = 32;

fn fail(message: &str) -> ! {
    eprintln!("lifeline-systemd: {message}");
    process::exit(1);
}

fn inherited_listener() -> Result<UnixListener, String> {
    let pid = env::var(SYSTEMD_LISTEN_PID)
        .map_err(|_| "must be started by lifeline-system.socket (missing LISTEN_PID)".to_string())?;
    if pid.parse::<u32>().ok() != Some(process::id()) {
        return Err("systemd socket activation PID does not match this process".to_string());
    }

    let count = env::var(SYSTEMD_LISTEN_FDS)
        .map_err(|_| "must be started by lifeline-system.socket (missing LISTEN_FDS)".to_string())?;
    if count != "1" {
        return Err(format!("expected exactly one activated socket, received {count}"));
    }

    // Validate descriptor 3 before transferring ownership from systemd.
    let fd = 3;
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
        return Err("systemd did not provide a valid socket descriptor".to_string());
    }

    let mut socket_type: libc::c_int = 0;
    let mut option_len = std::mem::size_of_val(&socket_type) as libc::socklen_t;
    let type_result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            &mut socket_type as *mut libc::c_int as *mut libc::c_void,
            &mut option_len,
        )
    };
    if type_result != 0 || socket_type != libc::SOCK_STREAM {
        return Err("activated descriptor is not a stream socket".to_string());
    }

    let mut accepting: libc::c_int = 0;
    option_len = std::mem::size_of_val(&accepting) as libc::socklen_t;
    let accept_result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ACCEPTCONN,
            &mut accepting as *mut libc::c_int as *mut libc::c_void,
            &mut option_len,
        )
    };
    if accept_result != 0 || accepting != 1 {
        return Err("activated stream socket is not listening".to_string());
    }

    // systemd passes the first listening socket at descriptor 3.
    let listener = unsafe { UnixListener::from_raw_fd(fd) };
    let address = listener
        .local_addr()
        .map_err(|e| format!("cannot inspect activated socket address: {e}"))?;
    if address.as_pathname() != Some(std::path::Path::new("/run/lifeline/control.sock")) {
        return Err("activated socket has an unexpected filesystem path".to_string());
    }
    Ok(listener)
}


fn handle_request<R: Read, W: Write>(reader: &mut R, writer: &mut W) -> Result<(), String> {
    let mut request = Vec::with_capacity(MAX_REQUEST_BYTES);
    let mut byte = [0_u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => return Err("client disconnected before sending a request".to_string()),
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => {
                if request.len() >= MAX_REQUEST_BYTES {
                    writer.write_all(b"ERROR request-too-long\n").ok();
                    return Err("rejected oversized request".to_string());
                }
                request.push(byte[0]);
            }
            Err(error) => return Err(format!("cannot read request: {error}")),
        }
    }

    match request.as_slice() {
        b"PING" => writer
            .write_all(b"PONG\n")
            .map_err(|e| format!("cannot send response: {e}")),
        b"STATUS" => writer
            .write_all(b"OK system-service=ready protocol=1\n")
            .map_err(|e| format!("cannot send response: {e}")),
        _ => {
            writer
                .write_all(b"ERROR unsupported-request\n")
                .map_err(|e| format!("cannot send response: {e}"))?;
            Err("rejected unsupported request".to_string())
        }
    }
}

fn handle_connection(mut stream: UnixStream) -> Result<(), String> {
    // The systemd socket's root:lifeline ownership and 0660 mode restrict
    // who can connect. This service currently exposes read-only health checks.
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| format!("cannot set request timeout: {e}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| format!("cannot set response timeout: {e}"))?;

    let mut reader = stream
        .try_clone()
        .map_err(|e| format!("cannot clone client socket: {e}"))?;
    handle_request(&mut reader, &mut stream)
}

fn main() {
    if unsafe { libc::geteuid() } != 0 {
        fail("must run as root through the system service");
    }

    let listener = inherited_listener().unwrap_or_else(|error| fail(&error));
    eprintln!("lifeline-systemd: privileged control socket is ready (read-only status protocol)");

    loop {
        match listener.accept() {
            Ok((stream, _address)) => {
                if let Err(error) = handle_connection(stream) {
                    eprintln!("lifeline-systemd: {error}");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                eprintln!("lifeline-systemd: accept failed: {error}");
                thread::sleep(Duration::from_millis(250));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn answers_ping() {
        let mut request = Cursor::new(b"PING\n".to_vec());
        let mut response = Vec::new();
        handle_request(&mut request, &mut response).unwrap();
        assert_eq!(response, b"PONG\n");
    }

    #[test]
    fn answers_status() {
        let mut request = Cursor::new(b"STATUS\n".to_vec());
        let mut response = Vec::new();
        handle_request(&mut request, &mut response).unwrap();
        assert_eq!(response, b"OK system-service=ready protocol=1\n");
    }

    #[test]
    fn rejects_unknown_requests() {
        let mut request = Cursor::new(b"RUN-SHELL\n".to_vec());
        let mut response = Vec::new();
        assert!(handle_request(&mut request, &mut response).is_err());
        assert_eq!(response, b"ERROR unsupported-request\n");
    }

    #[test]
    fn rejects_oversized_requests() {
        let mut request = Cursor::new(vec![b'A'; MAX_REQUEST_BYTES + 1]);
        let mut response = Vec::new();
        assert!(handle_request(&mut request, &mut response).is_err());
        assert_eq!(response, b"ERROR request-too-long\n");
    }
}
