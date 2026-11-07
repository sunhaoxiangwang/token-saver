//! token-saver binary entry point.

#![forbid(unsafe_code)]
#![deny(clippy::print_stdout, clippy::print_stderr)]

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use rmcp::ServiceExt;
use rmcp::service::ServerInitializeError;
use tokio_util::sync::CancellationToken;

use token_saver::diag::{Diag, Event, Level, Trigger};
use token_saver::limits;
use token_saver::probe::{SUPPORTED_VERSIONS_LABEL, TokenSaver, VERSION, new_instance_id};
use token_saver::supervisor::{self, Cause, Participant, ProcessExit, Shared};
use token_saver::transport::{BoundedStdio, Registry};

const USAGE: &str =
    "usage: token-saver serve [--diag off|error|info|debug]\n       token-saver --version\n";

enum Command {
    Serve { level: Level },
    Version,
    Usage { code: i32 },
}

fn parse(args: impl Iterator<Item = String>) -> Command {
    let args: Vec<String> = args.collect();
    match args.first().map(String::as_str) {
        Some("--version") | Some("-V") if args.len() == 1 => Command::Version,
        Some("--help") | Some("-h") if args.len() == 1 => Command::Usage { code: 0 },
        Some("serve") => {
            let mut level = Level::Info;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--diag" => {
                        let Some(value) = args.get(i + 1) else {
                            return Command::Usage { code: 2 };
                        };
                        let Some(parsed) = Level::parse(value) else {
                            return Command::Usage { code: 2 };
                        };
                        level = parsed;
                        i += 2;
                    }
                    _ => return Command::Usage { code: 2 },
                }
            }
            Command::Serve { level }
        }
        _ => Command::Usage { code: 2 },
    }
}

fn write_stdout(text: &str) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = lock.write_all(text.as_bytes());
    let _ = lock.flush();
}

fn write_stderr(text: &str) {
    let stderr = std::io::stderr();
    let mut lock = stderr.lock();
    let _ = lock.write_all(text.as_bytes());
    let _ = lock.flush();
}

#[cfg(unix)]
fn spawn_signal_tasks(shared: Arc<Shared>) {
    use tokio::signal::unix::{SignalKind, signal};
    {
        let shared = shared.clone();
        tokio::spawn(async move {
            if let Ok(mut s) = signal(SignalKind::interrupt()) {
                s.recv().await;
                shared.trigger_orderly(Trigger::Sigint);
                shared.token().cancel();
            }
        });
    }
    tokio::spawn(async move {
        if let Ok(mut s) = signal(SignalKind::terminate()) {
            s.recv().await;
            shared.trigger_orderly(Trigger::Sigterm);
            shared.token().cancel();
        }
    });
}

#[cfg(not(unix))]
fn spawn_signal_tasks(shared: Arc<Shared>) {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            shared.trigger_orderly(Trigger::Sigint);
            shared.token().cancel();
        }
    });
}

fn serve(level: Level) -> i32 {
    let diag = Diag::start(level);
    let ct = CancellationToken::new();
    let shared = Arc::new(Shared::new(ct.clone(), diag.clone()));
    shared.arm_txn(Participant::Startup, limits::STARTUP);
    supervisor::spawn(shared.clone(), Arc::new(ProcessExit), limits::TICK);
    let registry = Registry::new(shared.clone(), diag.clone());
    let instance_id = new_instance_id();
    diag.emit(Event::Startup {
        version: VERSION,
        instance_id: instance_id.clone(),
        supported_versions: SUPPORTED_VERSIONS_LABEL,
    });

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => {
            shared.latch_fatal(Cause::Invariant);
            return shared.exit_code(false);
        }
    };

    let code = runtime.block_on(async {
        spawn_signal_tasks(shared.clone());
        let transport = BoundedStdio::stdio(registry.clone());
        shared.clear_txn(Participant::Startup);
        let handler = TokenSaver::new(registry.clone(), instance_id);
        match handler.serve_with_ct(transport, ct.clone()).await {
            Ok(running) => {
                let _ = running.waiting().await;
            }
            Err(ServerInitializeError::ConnectionClosed(_)) => shared.trigger_orderly(Trigger::Eof),
            Err(ServerInitializeError::Cancelled) => {}
            Err(_) => {
                shared.latch_fatal(Cause::InitializeFailed);
            }
        }
        // Orderly success requires retired work: no unretired requests and no unacknowledged
        // deliveries. Wait for retirement until the shutdown budget; the supervisor forces exit 1
        // at that point, and this loop mirrors that outcome if it observes the deadline first.
        loop {
            if shared.is_fatal() {
                break shared.exit_code(false);
            }
            let counts = registry.counts();
            if counts.requests == 0 && counts.deliveries == 0 {
                break shared.exit_code(false);
            }
            let deadline = shared.shutdown_deadline_nanos();
            if deadline != 0 && shared.now_nanos() >= deadline {
                break shared.exit_code(true);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });

    let counts = registry.counts();
    diag.emit(Event::Shutdown {
        trigger: shared.orderly(),
        in_flight: counts.requests + counts.deliveries,
        code,
    });
    // Give the diagnostics thread one short, bounded chance to flush the shutdown line.
    std::thread::sleep(Duration::from_millis(20));
    code
}

fn main() {
    let code = match parse(std::env::args().skip(1)) {
        Command::Version => {
            write_stdout(&format!("token-saver {VERSION}\n"));
            0
        }
        Command::Usage { code } => {
            write_stderr(USAGE);
            code
        }
        Command::Serve { level } => serve(level),
    };
    std::process::exit(code)
}
