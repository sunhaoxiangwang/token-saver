//! Production-binary stall test with a harness-established saturated stdout pipe (R5-04).

mod common;

use std::os::fd::FromRawFd;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::*;

fn saturated_pipe() -> (i32, i32, usize) {
    let mut fds = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let (read_end, write_end) = (fds[0], fds[1]);
    set_nonblock(write_end, true);
    let chunk = vec![b'x'; 4096];
    let mut prefill = 0usize;
    loop {
        let n = unsafe { libc::write(write_end, chunk.as_ptr().cast(), chunk.len()) };
        if n > 0 {
            prefill += n as usize;
            continue;
        }
        let e = errno();
        assert!(
            e == libc::EAGAIN || e == libc::EWOULDBLOCK,
            "unexpected errno {e}"
        );
        break;
    }
    let one = *b"x";
    assert_eq!(
        unsafe { libc::write(write_end, one.as_ptr().cast(), 1) },
        -1
    );
    let e = errno();
    assert!(e == libc::EAGAIN || e == libc::EWOULDBLOCK);
    set_nonblock(write_end, false);
    assert_eq!(
        unsafe { libc::fcntl(write_end, libc::F_GETFL) } & libc::O_NONBLOCK,
        0
    );
    (read_end, write_end, prefill)
}

#[test]
fn eof_with_undelivered_output_is_forced_with_code_1_at_the_shutdown_budget() {
    let (read_end, write_end, prefill) = saturated_pipe();
    let child_stdout = unsafe { Stdio::from_raw_fd(write_end) };
    let mut child = Command::new(BIN)
        .args(["serve", "--diag", "off"])
        .stdin(Stdio::piped())
        .stdout(child_stdout)
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let mut stdin = child.stdin.take().unwrap();
    use std::io::Write;
    writeln!(stdin, "{}", discover(serde_json::json!(1), "2026-07-28")).unwrap();
    stdin.flush().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let t_eof = Instant::now();
    drop(stdin);
    let deadline = t_eof + Duration::from_secs(8);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let elapsed = t_eof.elapsed();
    let status = status.expect("server did not terminate");
    assert_eq!(
        exit_code(status),
        1,
        "orderly trigger with undelivered work is forced with code 1"
    );
    assert!(
        elapsed <= Duration::from_millis(3000 + 100 + 500),
        "{elapsed:?}"
    );
    assert!(elapsed >= Duration::from_millis(2500), "{elapsed:?}");
    set_nonblock(read_end, true);
    let mut buf = vec![0u8; 65_536];
    let mut drained = 0usize;
    loop {
        let n = unsafe { libc::read(read_end, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            drained += n as usize;
        } else {
            break;
        }
    }
    assert_eq!(drained, prefill);
    unsafe {
        libc::close(read_end);
    }
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn set_nonblock(fd: i32, on: bool) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        let new = if on {
            flags | libc::O_NONBLOCK
        } else {
            flags & !libc::O_NONBLOCK
        };
        assert_eq!(libc::fcntl(fd, libc::F_SETFL, new), 0);
    }
}

#[test]
fn saturated_stdout_pipe_triggers_delivery_deadline_exit_4() {
    let mut fds = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let (read_end, write_end) = (fds[0], fds[1]);

    // 1. Fill the pipe with nonblocking writes.
    set_nonblock(write_end, true);
    let chunk = vec![b'x'; 4096];
    let mut prefill = 0usize;
    loop {
        let n = unsafe { libc::write(write_end, chunk.as_ptr().cast(), chunk.len()) };
        if n > 0 {
            prefill += n as usize;
            continue;
        }
        let e = errno();
        assert!(
            e == libc::EAGAIN || e == libc::EWOULDBLOCK,
            "unexpected errno {e}"
        );
        break;
    }
    // 2. One-byte saturation proof.
    let one = *b"x";
    let n = unsafe { libc::write(write_end, one.as_ptr().cast(), 1) };
    assert_eq!(n, -1, "one more byte must not fit");
    let e = errno();
    assert!(
        e == libc::EAGAIN || e == libc::EWOULDBLOCK,
        "one-byte write errno {e}"
    );
    // 3. Restore blocking mode and verify it.
    set_nonblock(write_end, false);
    let flags = unsafe { libc::fcntl(write_end, libc::F_GETFL) };
    assert_eq!(flags & libc::O_NONBLOCK, 0, "blocking mode restored");

    // 4. Spawn with the saturated pipe as stdout; keep the read end open and undrained.
    let child_stdout = unsafe { Stdio::from_raw_fd(write_end) };
    let mut child = Command::new(BIN)
        .args(["serve", "--diag", "off"])
        .stdin(Stdio::piped())
        .stdout(child_stdout)
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let mut stdin = child.stdin.take().unwrap();
    use std::io::Write;
    let t_send = Instant::now();
    writeln!(stdin, "{}", discover(serde_json::json!(1), "2026-07-28")).unwrap();
    stdin.flush().unwrap();

    // 5. Wait with the harness's own timeout; kill and reap if the watchdog failed.
    let deadline = t_send + Duration::from_secs(8);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let elapsed = t_send.elapsed();
    let status = status.expect("server did not terminate: watchdog broken");
    assert_eq!(exit_code(status), 4, "delivery deadline exit code");
    assert!(
        elapsed <= Duration::from_millis(5000 + 100 + 1000),
        "terminated after {elapsed:?}"
    );
    assert!(
        elapsed >= Duration::from_secs(5),
        "terminated too early: {elapsed:?}"
    );

    // 6. Drain the read end: exactly the prefill, no server bytes.
    drop(stdin);
    set_nonblock(read_end, true);
    let mut drained = Vec::new();
    let mut buf = vec![0u8; 65_536];
    loop {
        let n = unsafe { libc::read(read_end, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            drained.extend_from_slice(&buf[..n as usize]);
        } else {
            break;
        }
    }
    assert_eq!(drained.len(), prefill, "pipe content changed size");
    assert!(
        drained.iter().all(|&b| b == b'x'),
        "server bytes reached a saturated pipe"
    );
    eprintln!("MEASURED: pipe absorbed {prefill} bytes before saturation on this OS");
    unsafe {
        libc::close(read_end);
    }
}
