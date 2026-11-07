//! In-process harness: production transport, registry, supervisor, with a controllable service
//! and a gated writer. Only the terminator and the service differ from production.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use rmcp::ErrorData as McpError;
use rmcp::ServerHandler;
use rmcp::ServiceExt;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, Implementation, ListToolsResult,
    PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerConfig,
};
use rmcp::service::{NotificationContext, RequestContext, RoleServer};
use serde_json::Value;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use token_saver::diag::Diag;
use token_saver::limits;
use token_saver::probe;
use token_saver::supervisor::{self, Cause, Shared, Terminator};
use token_saver::transport::{BoundedStdio, Registry};

/// Records the supervisor's termination decision instead of exiting.
#[derive(Default)]
pub struct RecordingTerminator {
    decision: Mutex<Option<(i32, Cause)>>,
    cv: Condvar,
}

impl RecordingTerminator {
    pub fn wait(&self, timeout: Duration) -> Option<(i32, Cause)> {
        let deadline = Instant::now() + timeout;
        let mut guard = self.decision.lock().unwrap();
        while guard.is_none() {
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let (g, _) = self.cv.wait_timeout(guard, deadline - now).unwrap();
            guard = g;
        }
        *guard
    }

    pub fn decided(&self) -> Option<(i32, Cause)> {
        *self.decision.lock().unwrap()
    }
}

impl Terminator for RecordingTerminator {
    fn terminate(&self, code: i32, cause: Cause) {
        let mut guard = self.decision.lock().unwrap();
        if guard.is_none() {
            *guard = Some((code, cause));
        }
        self.cv.notify_all();
    }
}

/// Per-nonce gates that hold tool calls until released.
#[derive(Default)]
pub struct Gate {
    inner: Mutex<HashMap<String, watch::Sender<bool>>>,
}

impl Gate {
    fn subscribe(&self, key: &str) -> watch::Receiver<bool> {
        let mut map = self.inner.lock().unwrap();
        map.entry(key.to_owned())
            .or_insert_with(|| watch::channel(false).0)
            .subscribe()
    }

    pub fn release(&self, key: &str) {
        let mut map = self.inner.lock().unwrap();
        let tx = map
            .entry(key.to_owned())
            .or_insert_with(|| watch::channel(false).0);
        let _ = tx.send(true);
    }

    pub fn release_all(&self) {
        for tx in self.inner.lock().unwrap().values() {
            let _ = tx.send(true);
        }
    }

    async fn wait(rx: &mut watch::Receiver<bool>) {
        while !*rx.borrow() {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HoldMode {
    /// Observes the registry token and the processing deadline.
    Cooperative,
    /// Ignores every cancellation signal until released.
    Resistant,
    /// Blocks the runtime thread for the given seconds (simulates a non-yielding stall).
    BlockThread(u64),
}

/// Service whose `connection_probe` holds when the nonce starts with `hold`.
#[derive(Clone)]
pub struct GateService {
    registry: Arc<Registry>,
    gate: Arc<Gate>,
    mode: HoldMode,
    pub calls: Arc<AtomicUsize>,
    pub initialized_calls: Arc<AtomicUsize>,
    pub notifications: Arc<AtomicUsize>,
    instance_id: String,
}

impl GateService {
    pub fn new(registry: Arc<Registry>, gate: Arc<Gate>, mode: HoldMode) -> GateService {
        GateService {
            registry,
            gate,
            mode,
            calls: Arc::new(AtomicUsize::new(0)),
            initialized_calls: Arc::new(AtomicUsize::new(0)),
            notifications: Arc::new(AtomicUsize::new(0)),
            instance_id: probe::new_instance_id(),
        }
    }
}

impl ServerHandler for GateService {
    fn get_info(&self) -> ServerConfig {
        let mut config = ServerConfig::new(ServerCapabilities::builder().enable_tools().build());
        config.server_info = Implementation::new("token-saver-fixture", probe::VERSION);
        config
    }

    fn supported_protocol_versions(&self) -> std::borrow::Cow<'static, [ProtocolVersion]> {
        std::borrow::Cow::Borrowed(probe::supported_versions())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(vec![probe::tool()]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let nonce = request
            .arguments
            .as_ref()
            .and_then(|a| a.get("nonce"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let ctx = self.registry.handler_context(&context.id);
        let instance_id = self.instance_id.clone();
        let args = request.arguments.clone();
        let result = if nonce.starts_with("hold") {
            match self.mode {
                HoldMode::Cooperative => {
                    let mut rx = self.gate.subscribe(&nonce);
                    let work = async move {
                        Gate::wait(&mut rx).await;
                        probe::evaluate(&instance_id, args.as_ref())
                            .map_err(|_| McpError::internal_error(probe::ERR_INTERNAL, None))
                    };
                    probe::run_with_deadline(ctx, context.ct.clone(), work).await
                }
                HoldMode::Resistant => {
                    let mut rx = self.gate.subscribe(&nonce);
                    Gate::wait(&mut rx).await;
                    probe::evaluate(&instance_id, args.as_ref())
                        .map_err(|_| McpError::internal_error(probe::ERR_INTERNAL, None))
                }
                HoldMode::BlockThread(secs) => {
                    std::thread::sleep(Duration::from_secs(secs));
                    probe::evaluate(&instance_id, args.as_ref())
                        .map_err(|_| McpError::internal_error(probe::ERR_INTERNAL, None))
                }
            }
        } else {
            let work = async move {
                probe::evaluate(&instance_id, args.as_ref())
                    .map_err(|_| McpError::internal_error(probe::ERR_INTERNAL, None))
            };
            probe::run_with_deadline(ctx, context.ct.clone(), work).await
        };
        self.registry.handler_finished(&context.id);
        result.map(Into::into)
    }

    async fn on_initialized(&self, _context: NotificationContext<RoleServer>) {
        self.initialized_calls.fetch_add(1, Ordering::SeqCst);
        self.registry.initialized_done();
    }
}

/// A writer that can be made to block inside `write` or `flush` until released.
pub struct WriteGate {
    block_write: AtomicBool,
    block_flush: AtomicBool,
    released: Mutex<bool>,
    cv: Condvar,
    write_began: AtomicBool,
    flush_began: AtomicBool,
    fail_after_release: AtomicBool,
}

impl Default for WriteGate {
    fn default() -> Self {
        WriteGate {
            block_write: AtomicBool::new(false),
            block_flush: AtomicBool::new(false),
            released: Mutex::new(false),
            cv: Condvar::new(),
            write_began: AtomicBool::new(false),
            flush_began: AtomicBool::new(false),
            fail_after_release: AtomicBool::new(false),
        }
    }
}

impl WriteGate {
    pub fn block_writes(&self) {
        self.block_write.store(true, Ordering::SeqCst);
    }
    pub fn block_flushes(&self) {
        self.block_flush.store(true, Ordering::SeqCst);
    }
    pub fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.cv.notify_all();
    }
    /// Releases the blocked call and makes it fail with a broken pipe.
    pub fn fail_and_release(&self) {
        self.fail_after_release.store(true, Ordering::SeqCst);
        self.release();
    }
    pub fn write_began(&self) -> bool {
        self.write_began.load(Ordering::SeqCst)
    }
    pub fn flush_began(&self) -> bool {
        self.flush_began.load(Ordering::SeqCst)
    }
    pub fn wait_began(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.write_began() || self.flush_began() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }
    fn wait_release(&self) {
        let mut g = self.released.lock().unwrap();
        while !*g {
            g = self.cv.wait(g).unwrap();
        }
    }
}

pub struct GatedWriter<W: Write> {
    inner: W,
    gate: Arc<WriteGate>,
}

impl<W: Write> Write for GatedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.gate.block_write.load(Ordering::SeqCst) {
            self.gate.write_began.store(true, Ordering::SeqCst);
            self.gate.wait_release();
            if self.gate.fail_after_release.load(Ordering::SeqCst) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "gated failure",
                ));
            }
        }
        self.inner.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if self.gate.block_flush.load(Ordering::SeqCst) {
            self.gate.flush_began.store(true, Ordering::SeqCst);
            self.gate.wait_release();
        }
        self.inner.flush()
    }
}

/// The in-process harness.
pub struct Harness {
    pub registry: Arc<Registry>,
    pub shared: Arc<Shared>,
    pub terminator: Arc<RecordingTerminator>,
    pub gate: Arc<Gate>,
    pub write_gate: Arc<WriteGate>,
    pub diag: Diag,
    client_in: Option<std::io::PipeWriter>,
    client_out: Receiver<Vec<u8>>,
    service_done: Receiver<()>,
    pub service: GateService,
    _idle_transport: Option<BoundedStdio>,
    _done_tx: Option<std::sync::mpsc::Sender<()>>,
    pub runtime: Option<tokio::runtime::Handle>,
}

impl Harness {
    pub fn start(mode: HoldMode) -> Harness {
        let diag = Diag::disabled();
        let ct = CancellationToken::new();
        let shared = Arc::new(Shared::new(ct.clone(), diag.clone()));
        let terminator = Arc::new(RecordingTerminator::default());
        supervisor::spawn(shared.clone(), terminator.clone(), limits::TICK);
        let registry = Registry::new(shared.clone(), diag.clone());
        let gate = Arc::new(Gate::default());
        let write_gate = Arc::new(WriteGate::default());
        let service = GateService::new(registry.clone(), gate.clone(), mode);

        let (client_to_server_r, client_to_server_w) = std::io::pipe().expect("pipe");
        let (server_to_client_r, server_to_client_w) = std::io::pipe().expect("pipe");
        let writer = GatedWriter {
            inner: server_to_client_w,
            gate: write_gate.clone(),
        };
        let transport = BoundedStdio::spawn(client_to_server_r, writer, registry.clone());
        let (done_tx, done_rx) = channel();
        let (handle_tx, handle_rx) = channel();
        let svc = service.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let _ = handle_tx.send(rt.handle().clone());
            rt.block_on(async move {
                if let Ok(running) = svc.serve_with_ct(transport, ct).await {
                    let _ = running.waiting().await;
                }
            });
            let _ = done_tx.send(());
        });
        let client_out = pump(server_to_client_r);
        Harness {
            registry,
            shared,
            terminator,
            gate,
            write_gate,
            diag,
            client_in: Some(client_to_server_w),
            client_out,
            service_done: done_rx,
            service,
            _idle_transport: None,
            _done_tx: None,
            runtime: handle_rx.recv().ok(),
        }
    }

    /// Reader, writer, registry, and supervisor run, but no service ever dispatches requests.
    /// Models a runtime that never reaches `receive`.
    pub fn start_without_service() -> Harness {
        let diag = Diag::disabled();
        let ct = CancellationToken::new();
        let shared = Arc::new(Shared::new(ct.clone(), diag.clone()));
        let terminator = Arc::new(RecordingTerminator::default());
        supervisor::spawn(shared.clone(), terminator.clone(), limits::TICK);
        let registry = Registry::new(shared.clone(), diag.clone());
        let gate = Arc::new(Gate::default());
        let write_gate = Arc::new(WriteGate::default());
        let service = GateService::new(registry.clone(), gate.clone(), HoldMode::Cooperative);
        let (client_to_server_r, client_to_server_w) = std::io::pipe().expect("pipe");
        let (server_to_client_r, server_to_client_w) = std::io::pipe().expect("pipe");
        let writer = GatedWriter {
            inner: server_to_client_w,
            gate: write_gate.clone(),
        };
        let transport = BoundedStdio::spawn(client_to_server_r, writer, registry.clone());
        let (done_tx, done_rx) = channel();
        let client_out = pump(server_to_client_r);
        Harness {
            registry,
            shared,
            terminator,
            gate,
            write_gate,
            diag,
            client_in: Some(client_to_server_w),
            client_out,
            service_done: done_rx,
            service,
            _idle_transport: Some(transport),
            _done_tx: Some(done_tx),
            runtime: None,
        }
    }

    pub fn send(&mut self, value: Value) {
        self.send_line(&value.to_string());
    }

    pub fn send_line(&mut self, line: &str) {
        if let Some(w) = self.client_in.as_mut() {
            let _ = w.write_all(line.as_bytes());
            let _ = w.write_all(b"\n");
            let _ = w.flush();
        }
    }

    pub fn send_raw(&mut self, bytes: &[u8]) {
        if let Some(w) = self.client_in.as_mut() {
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
    }

    pub fn recv(&mut self, timeout: Duration) -> Option<Value> {
        let line = self.client_out.recv_timeout(timeout).ok()?;
        Some(serde_json::from_slice(&line).expect("server output is JSON"))
    }

    pub fn recv_expect(&mut self) -> Value {
        self.recv(Duration::from_secs(5))
            .expect("expected a frame within 5 s")
    }

    pub fn assert_silent(&mut self, wait: Duration) {
        if let Some(v) = self.recv(wait) {
            panic!("unexpected frame: {v}");
        }
    }

    pub fn close_stdin(&mut self) {
        self.client_in.take();
    }

    pub fn wait_service_end(&self, timeout: Duration) -> bool {
        self.service_done.recv_timeout(timeout).is_ok()
    }

    /// Waits until the registry reports the given admitted count.
    pub fn wait_admitted(&self, n: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.registry.counts().admitted == n {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    pub fn wait_requests(&self, n: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.registry.counts().requests == n {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    pub fn wait_fatal(&self, timeout: Duration) -> Cause {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let c = self.shared.fatal();
            if c != Cause::None {
                return c;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Cause::None
    }
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
