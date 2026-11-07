//! Shared test support: a raw stdio client for the production binary (SDK-independent) and an
//! in-process harness running the production transport, registry, and supervisor with a
//! controllable service and writer.

#![allow(dead_code)]

pub mod fixture;

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

pub const BIN: &str = env!("CARGO_BIN_EXE_token-saver");

pub fn meta(version: &str) -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": version,
        "io.modelcontextprotocol/clientInfo": {"name": "raw-test-client", "version": "0"},
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

pub fn initialize(id: Value, version: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"initialize","params":{"protocolVersion":version,"capabilities":{},"clientInfo":{"name":"raw-test-client","version":"0"}}})
}

pub fn initialized() -> Value {
    json!({"jsonrpc":"2.0","method":"notifications/initialized"})
}

pub fn discover(id: Value, version: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"server/discover","params":{"_meta":meta(version)}})
}

pub fn tools_list(id: Value, modern: bool) -> Value {
    if modern {
        json!({"jsonrpc":"2.0","id":id,"method":"tools/list","params":{"_meta":meta("2026-07-28")}})
    } else {
        json!({"jsonrpc":"2.0","id":id,"method":"tools/list"})
    }
}

pub fn tools_call(id: Value, arguments: Value, modern: bool) -> Value {
    let mut params = json!({"name":"connection_probe","arguments":arguments});
    if modern {
        params["_meta"] = meta("2026-07-28");
    }
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":params})
}

pub fn probe_call(id: Value, nonce: &str, modern: bool) -> Value {
    tools_call(id, json!({"nonce": nonce}), modern)
}

pub fn cancel(id: Value) -> Value {
    json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":id,"reason":"test"}})
}

fn pump<R: Read + Send + 'static>(reader: R) -> Receiver<Vec<u8>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        loop {
            let mut buf = Vec::new();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx.send(buf).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

/// The production binary under test, driven over raw pipes.
pub struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Receiver<Vec<u8>>,
    stderr: Receiver<Vec<u8>>,
    stderr_lines: Vec<String>,
    stdout_raw: Vec<Vec<u8>>,
}

impl Server {
    pub fn spawn() -> Server {
        Server::spawn_with(&["serve", "--diag", "debug"], &[])
    }

    pub fn spawn_with(args: &[&str], env: &[(&str, &str)]) -> Server {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawn token-saver");
        let stdin = child.stdin.take();
        let stdout = pump(child.stdout.take().expect("stdout"));
        let stderr = pump(child.stderr.take().expect("stderr"));
        Server {
            child,
            stdin,
            stdout,
            stderr,
            stderr_lines: Vec::new(),
            stdout_raw: Vec::new(),
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn send_raw(&mut self, bytes: &[u8]) {
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = stdin.write_all(bytes);
            let _ = stdin.flush();
        }
    }

    pub fn send_line(&mut self, line: &str) {
        let mut bytes = line.as_bytes().to_vec();
        bytes.push(b'\n');
        self.send_raw(&bytes);
    }

    pub fn send(&mut self, value: Value) {
        self.send_line(&value.to_string());
    }

    pub fn recv_raw(&mut self, timeout: Duration) -> Option<Vec<u8>> {
        match self.stdout.recv_timeout(timeout) {
            Ok(line) => {
                self.stdout_raw.push(line.clone());
                Some(line)
            }
            Err(_) => None,
        }
    }

    pub fn recv(&mut self, timeout: Duration) -> Option<Value> {
        let line = self.recv_raw(timeout)?;
        Some(serde_json::from_slice(&line).unwrap_or_else(|e| {
            panic!(
                "stdout line is not JSON ({e}): {:?}",
                String::from_utf8_lossy(&line)
            )
        }))
    }

    pub fn recv_expect(&mut self) -> Value {
        self.recv(Duration::from_secs(5))
            .expect("expected a response within 5 s")
    }

    /// Asserts that nothing arrives on stdout within `wait`.
    pub fn assert_silent(&mut self, wait: Duration) {
        if let Some(line) = self.recv_raw(wait) {
            panic!(
                "unexpected stdout line: {:?}",
                String::from_utf8_lossy(&line)
            );
        }
    }

    pub fn close_stdin(&mut self) {
        self.stdin.take();
    }

    pub fn wait_exit(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Ok(None) => return None,
                Err(_) => return None,
            }
        }
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn signal(&self, sig: i32) {
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, sig);
        }
    }

    /// Drains and returns every stderr line seen so far.
    pub fn stderr_lines(&mut self) -> Vec<String> {
        while let Ok(line) = self.stderr.try_recv() {
            self.stderr_lines
                .push(String::from_utf8_lossy(&line).trim_end().to_owned());
        }
        self.stderr_lines.clone()
    }

    /// Waits until stderr has been fully drained after exit.
    pub fn stderr_after_exit(&mut self) -> Vec<String> {
        while let Ok(line) = self.stderr.recv_timeout(Duration::from_millis(200)) {
            self.stderr_lines
                .push(String::from_utf8_lossy(&line).trim_end().to_owned());
        }
        self.stderr_lines.clone()
    }

    pub fn startup_instance_id(&mut self) -> Option<String> {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            for line in self.stderr_lines() {
                if let Some(idx) = line.find("instance_id=") {
                    return Some(
                        line[idx + "instance_id=".len()..]
                            .split(' ')
                            .next()
                            .unwrap()
                            .to_owned(),
                    );
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    pub fn all_stdout_raw(&self) -> &[Vec<u8>] {
        &self.stdout_raw
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Performs the legacy handshake and returns the initialize result.
pub fn legacy_handshake(server: &mut Server, version: &str) -> Value {
    server.send(initialize(json!(1), version));
    let resp = server.recv_expect();
    server.send(initialized());
    resp
}

pub fn exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

pub fn compact(v: &Value) -> String {
    serde_json::to_string(v).unwrap()
}
