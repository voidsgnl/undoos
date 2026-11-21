//! Root-owned system service for the privileged recovery boundary.
//!
//! The service exposes fixed health checks and administrator-authorized,
//! exact-path capture and restore operations. It does not accept arbitrary
//! paths or commands; its initial recovery adapter is restricted to /etc/hostname.

use std::{
    env,
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    os::unix::net::{UnixListener, UnixStream},
    process, thread,
    time::Duration,
};

const SYSTEMD_LISTEN_FDS: &str = "LISTEN_FDS";
const SYSTEMD_LISTEN_PID: &str = "LISTEN_PID";
const MAX_REQUEST_BYTES: usize = 64;

#[path = "system_storage.rs"]
mod system_storage;

#[path = "system_store.rs"]
mod system_store;

#[path = "system_catalog.rs"]
mod system_catalog;

#[path = "system_capture.rs"]
mod system_capture;

#[path = "system_reconcile.rs"]
mod system_reconcile;

#[path = "system_restore.rs"]
mod system_restore;

#[derive(Debug, PartialEq, Eq)]
struct PeerCredentials {
    pid: libc::pid_t,
    uid: libc::uid_t,
    gid: libc::gid_t,
}

/// Read kernel-authenticated credentials for the process at the other end
/// of a connected Unix stream socket.
fn peer_credentials(stream: &UnixStream) -> Result<PeerCredentials, String> {
    let mut credentials = std::mem::MaybeUninit::<libc::ucred>::uninit();
    let mut credentials_len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;

    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            credentials.as_mut_ptr() as *mut libc::c_void,
            &mut credentials_len,
        )
    };

    if result != 0 {
        return Err(format!(
            "cannot authenticate local client credentials: {}",
            std::io::Error::last_os_error()
        ));
    }
    if credentials_len as usize != std::mem::size_of::<libc::ucred>() {
        return Err("kernel returned incomplete local client credentials".to_string());
    }

    // SAFETY: getsockopt succeeded and returned exactly sizeof(ucred) bytes.
    let credentials = unsafe { credentials.assume_init() };
    if credentials.pid <= 0 {
        return Err("kernel returned an invalid local client process ID".to_string());
    }

    Ok(PeerCredentials {
        pid: credentials.pid,
        uid: credentials.uid,
        gid: credentials.gid,
    })
}

// This action is deliberately specific to restoring protected system files.
// Other recovery domains must receive separate Polkit action IDs.
const SYSTEM_FILE_RECOVERY_ACTION: &str = "org.undoos.system.recover-file";
const SYSTEM_FILE_CAPTURE_ACTION: &str = "org.undoos.system.capture-file";

/// Extract Linux process start time (field 22 of /proc/<pid>/stat).
/// The command name is parenthesized and may itself contain spaces or ')'.
fn process_start_time(expected_pid: libc::pid_t, stat: &str) -> Result<u64, String> {
    let close_paren = stat
        .rfind(')')
        .ok_or_else(|| "cannot parse client process information".to_string())?;
    let reported_pid = stat[..stat.find(' ').unwrap_or(stat.len())]
        .parse::<libc::pid_t>()
        .map_err(|_| "cannot parse client process ID".to_string())?;
    if reported_pid != expected_pid {
        return Err("client process identity changed during authorization".to_string());
    }

    stat[close_paren + 1..]
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| "client process information is incomplete".to_string())?
        .parse::<u64>()
        .map_err(|_| "cannot parse client process start time".to_string())
}

/// Ask Polkit whether this exact kernel-identified process may restore a
/// protected system file. This is not called by the current read-only API.
#[allow(dead_code)]
fn authorize_system_file_recovery(peer: &PeerCredentials) -> Result<(), String> {
    authorize_action(peer, SYSTEM_FILE_RECOVERY_ACTION)
}

fn authorize_system_file_capture(peer: &PeerCredentials) -> Result<(), String> {
    authorize_action(peer, SYSTEM_FILE_CAPTURE_ACTION)
}

fn authorize_action(peer: &PeerCredentials, action_id: &str) -> Result<(), String> {
    let stat_path = format!("/proc/{}/stat", peer.pid);
    let stat = std::fs::read_to_string(&stat_path)
        .map_err(|error| format!("cannot verify client process identity: {error}"))?;
    let start_time = process_start_time(peer.pid, &stat)?;
    let subject = format!("{},{},{}", peer.pid, start_time, peer.uid);

    let output = process::Command::new("/usr/bin/pkcheck")
        .args([
            "--action-id",
            action_id,
            "--process",
            &subject,
            "--allow-user-interaction",
        ])
        .output()
        .map_err(|error| format!("cannot start Polkit authorization check: {error}"))?;

    if output.status.success() {
        return Ok(());
    }

    match output.status.code() {
        Some(1) => Err("Polkit denied the requested system-file operation".to_string()),
        Some(2) => Err(
            "Polkit could not authorize the request: no suitable authentication agent is available"
                .to_string(),
        ),
        Some(3) => Err("system-file authorization was dismissed".to_string()),
        code => Err(format!(
            "Polkit authorization failed (exit code {code:?}): {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
    }
}

fn fail(message: &str) -> ! {
    eprintln!("lifeline-systemd: {message}");
    process::exit(1);
}

fn inherited_listener() -> Result<UnixListener, String> {
    let pid = env::var(SYSTEMD_LISTEN_PID).map_err(|_| {
        "must be started by lifeline-system.socket (missing LISTEN_PID)".to_string()
    })?;
    if pid.parse::<u32>().ok() != Some(process::id()) {
        return Err("systemd socket activation PID does not match this process".to_string());
    }

    let count = env::var(SYSTEMD_LISTEN_FDS).map_err(|_| {
        "must be started by lifeline-system.socket (missing LISTEN_FDS)".to_string()
    })?;
    if count != "1" {
        return Err(format!(
            "expected exactly one activated socket, received {count}"
        ));
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

fn handle_request<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    peer: Option<&PeerCredentials>,
    storage_root: Option<&std::path::Path>,
) -> Result<(), String> {
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
        b"HISTORY-HOSTNAME" => {
            let records = system_catalog::read_all(
                storage_root.ok_or_else(|| "history request has no protected storage root".to_string())?
            )?;
            let mut hostname_records = records
                .into_iter()
                .filter(|record| record.original_path == std::path::Path::new("/etc/hostname"))
                .collect::<Vec<_>>();
            hostname_records.sort_by_key(|record| record.captured_at_ns);
            let start = hostname_records.len().saturating_sub(20);
            let selected = &hostname_records[start..];
            let mut response = format!("OK history count={}\n", selected.len());
            for record in selected {
                use std::fmt::Write as _;
                writeln!(
                    &mut response,
                    "{}|{}|{}",
                    record.object_id, record.captured_at_ns, record.original_size
                )
                .expect("writing to a String cannot fail");
            }
            writer
                .write_all(response.as_bytes())
                .map_err(|error| format!("cannot send system history: {error}"))
        }
        request if request.starts_with(b"RESTORE-HOSTNAME ") => {
            let peer = peer
                .ok_or_else(|| "restore request has no authenticated client".to_string())?;
            let storage_root = storage_root
                .ok_or_else(|| "restore request has no protected storage root".to_string())?;
            let object_id = std::str::from_utf8(&request[b"RESTORE-HOSTNAME ".len()..])
                .map_err(|_| "restore request contains an invalid object ID".to_string())?;
            let result = authorize_system_file_recovery(peer)
                .and_then(|()| system_restore::restore_hostname(storage_root, object_id));
            match result {
                Ok(backup_id) => {
                    let response = format!("OK restored backup={backup_id}\n");
                    writer
                        .write_all(response.as_bytes())
                        .map_err(|error| format!("cannot send restore response: {error}"))
                }
                Err(error) => {
                    eprintln!("lifeline-systemd: system-file restore failed: {error}");
                    writer
                        .write_all(b"ERROR restore-failed\n")
                        .map_err(|write_error| format!("cannot send restore error: {write_error}"))?;
                    Err("system-file restore request failed".to_string())
                }
            }
        }
        b"CAPTURE-HOSTNAME" => {
            let result = (|| {
                let peer = peer
                    .ok_or_else(|| "capture request has no authenticated client".to_string())?;
                let storage_root = storage_root
                    .ok_or_else(|| "capture request has no protected storage root".to_string())?;
                authorize_system_file_capture(peer)?;
                let policy = system_capture::CapturePolicy::new(
                    std::path::Path::new("/etc"),
                    vec![std::path::PathBuf::from("hostname")],
                )?;
                system_capture::capture(
                    storage_root,
                    &policy,
                    std::path::Path::new("hostname"),
                )
            })();

            match result {
                Ok(record) => {
                    let response = format!(
                        "OK captured object={} size={} path=/etc/hostname\n",
                        record.object_id, record.original_size
                    );
                    writer
                        .write_all(response.as_bytes())
                        .map_err(|e| format!("cannot send capture response: {e}"))
                }
                Err(error) => {
                    eprintln!("lifeline-systemd: system-file capture failed: {error}");
                    writer
                        .write_all(b"ERROR capture-failed\n")
                        .map_err(|e| format!("cannot send capture error response: {e}"))?;
                    Err("system-file capture request failed".to_string())
                }
            }
        }
        _ => {
            writer
                .write_all(b"ERROR unsupported-request\n")
                .map_err(|e| format!("cannot send response: {e}"))?;
            Err("rejected unsupported request".to_string())
        }
    }
}

fn handle_connection(mut stream: UnixStream) -> Result<(), String> {
    // SO_PEERCRED is supplied by the kernel, not by client-controlled request
    // data. The socket's root:lifeline ownership and 0660 mode enforce the
    // connection allowlist; these credentials identify the accepted client.
    let peer = peer_credentials(&stream)?;
    eprintln!(
        "lifeline-systemd: authenticated local client pid={} uid={} gid={}",
        peer.pid, peer.uid, peer.gid
    );

    // Health and history are read-only; capture and restore are separately authorized and path-bound.
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| format!("cannot set request timeout: {e}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| format!("cannot set response timeout: {e}"))?;

    let mut reader = stream
        .try_clone()
        .map_err(|e| format!("cannot clone client socket: {e}"))?;
    handle_request(
        &mut reader,
        &mut stream,
        Some(&peer),
        Some(std::path::Path::new("/var/lib/lifeline/system")),
    )
}

fn main() {
    if unsafe { libc::geteuid() } != 0 {
        fail("must run as root through the system service");
    }

    let storage_path = system_storage::verify()
        .unwrap_or_else(|error| fail(&format!("system recovery storage is not secure: {error}")));
    system_store::initialize(&storage_path).unwrap_or_else(|error| {
        fail(&format!(
            "cannot initialize protected system object storage: {error}"
        ))
    });
    let reconciliation = system_reconcile::reconcile(&storage_path).unwrap_or_else(|error| {
        fail(&format!(
            "system recovery storage reconciliation failed: {error}"
        ))
    });
    system_restore::recover_interrupted_restore(&storage_path)
        .unwrap_or_else(|error| fail(&format!("cannot recover interrupted system-file restore: {error}")));
    eprintln!(
        "lifeline-systemd: recovery store verified: {} catalog records; {} orphaned objects; {} interrupted temporary files",
        reconciliation.catalog_records,
        reconciliation.orphaned_object_ids.len(),
        reconciliation.interrupted_temporary_ids.len()
    );
    eprintln!(
        "lifeline-systemd: verified protected system storage at {}",
        storage_path.display()
    );

    let listener = inherited_listener().unwrap_or_else(|error| fail(&error));
    eprintln!("lifeline-systemd: privileged control socket is ready");

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
    fn reads_kernel_authenticated_peer_credentials() {
        let (server, _client) = UnixStream::pair().unwrap();
        let peer = peer_credentials(&server).unwrap();

        assert_eq!(peer.pid, unsafe { libc::getpid() });
        assert_eq!(peer.uid, unsafe { libc::geteuid() });
        assert_eq!(peer.gid, unsafe { libc::getegid() });
    }

    #[test]
    fn parses_process_start_time_with_spaces_and_closing_parentheses_in_name() {
        // Fields after the final ')' begin with field 3 (state); starttime is field 22.
        let stat =
            "123 (a tricky ) process) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 987654 22 23";
        assert_eq!(process_start_time(123, stat).unwrap(), 987654);
    }

    #[test]
    fn rejects_a_different_pid_in_process_stat() {
        let stat = "456 (client) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 987654";
        assert!(process_start_time(123, stat).is_err());
    }

    #[test]
    fn answers_ping() {
        let mut request = Cursor::new(b"PING\n".to_vec());
        let mut response = Vec::new();
        handle_request(&mut request, &mut response, None, None).unwrap();
        assert_eq!(response, b"PONG\n");
    }

    #[test]
    fn answers_status() {
        let mut request = Cursor::new(b"STATUS\n".to_vec());
        let mut response = Vec::new();
        handle_request(&mut request, &mut response, None, None).unwrap();
        assert_eq!(response, b"OK system-service=ready protocol=1\n");
    }

    #[test]
    fn refuses_system_capture_without_authenticated_context() {
        let mut request = Cursor::new(b"CAPTURE-HOSTNAME\n".to_vec());
        let mut response = Vec::new();
        let error = handle_request(&mut request, &mut response, None, None).unwrap_err();
        assert!(error.contains("capture request failed"));
        assert_eq!(response, b"ERROR capture-failed\n");
    }

    #[test]
    fn rejects_unknown_requests() {
        let mut request = Cursor::new(b"RUN-SHELL\n".to_vec());
        let mut response = Vec::new();
        assert!(handle_request(&mut request, &mut response, None, None).is_err());
        assert_eq!(response, b"ERROR unsupported-request\n");
    }

    #[test]
    fn rejects_client_supplied_system_paths() {
        let mut request = Cursor::new(b"CAPTURE /etc/passwd\n".to_vec());
        let mut response = Vec::new();
        assert!(handle_request(&mut request, &mut response, None, None).is_err());
        assert_eq!(response, b"ERROR unsupported-request\n");
    }

    #[test]
    fn rejects_oversized_requests() {
        let mut request = Cursor::new(vec![b'A'; MAX_REQUEST_BYTES + 1]);
        let mut response = Vec::new();
        assert!(handle_request(&mut request, &mut response, None, None).is_err());
        assert_eq!(response, b"ERROR request-too-long\n");
    }
}
