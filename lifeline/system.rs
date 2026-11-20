//! Minimal root-owned system service for the future privileged recovery boundary.
//!
//! This first version deliberately supports only a root-authenticated PING/PONG
//! health check. It does not inspect files or perform privileged mutations.

use std::{
    env,
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd},
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

    // systemd passes the first listening socket at file descriptor 3.
    // The descriptor is transferred into Rust ownership here.
    let listener = unsafe { UnixListener::from_raw_fd(3) };
    Ok(listener)
}

fn peer_is_root(stream: &UnixStream) -> Result<bool, String> {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut credentials as *mut libc::ucred as *mut libc::c_void,
            &mut length,
        )
    };
    if result != 0 {
        return Err(format!(
            "cannot verify connecting process credentials: {}",
            std::io::Error::last_os_error()
        ));
    }
    if length as usize != std::mem::size_of::<libc::ucred>() {
        return Err("kernel returned incomplete peer credentials".to_string());
    }
    Ok(credentials.uid == 0)
}

fn handle_connection(mut stream: UnixStream) -> Result<(), String> {
    if !peer_is_root(&stream)? {
        let _ = stream.write_all(b"DENIED\n");
        return Err("rejected connection from a non-root process".to_string());
    }

    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| format!("cannot set request timeout: {e}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| format!("cannot set response timeout: {e}"))?;

    let mut request = Vec::with_capacity(MAX_REQUEST_BYTES);
    let mut byte = [0_u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => return Err("client disconnected before sending a request".to_string()),
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => {
                if request.len() >= MAX_REQUEST_BYTES {
                    stream.write_all(b"ERROR request-too-long\n").ok();
                    return Err("rejected oversized request".to_string());
                }
                request.push(byte[0]);
            }
            Err(error) => return Err(format!("cannot read request: {error}")),
        }
    }

    match request.as_slice() {
        b"PING" => stream
            .write_all(b"PONG\n")
            .map_err(|e| format!("cannot send response: {e}")),
        _ => {
            stream
                .write_all(b"ERROR unsupported-request\n")
                .map_err(|e| format!("cannot send response: {e}"))?;
            Err("rejected unsupported request".to_string())
        }
    }
}

fn main() {
    if unsafe { libc::geteuid() } != 0 {
        fail("must run as root through the system service");
    }

    let listener = inherited_listener().unwrap_or_else(|error| fail(&error));
    eprintln!("lifeline-systemd: privileged control socket is ready (health-check only)");

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
    use std::os::unix::net::UnixStream;

    #[test]
    fn accepts_only_ping_requests_from_root_peer() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || handle_connection(server));
        client.write_all(b"PING\n").unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert_eq!(response, "PONG\n");
        assert!(worker.join().unwrap().is_ok());
    }

    #[test]
    fn rejects_unknown_requests() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || handle_connection(server));
        client.write_all(b"RUN-SHELL\n").unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert_eq!(response, "ERROR unsupported-request\n");
        assert!(worker.join().unwrap().is_err());
    }
}
