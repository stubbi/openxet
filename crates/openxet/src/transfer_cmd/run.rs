//! Running a transfer: the runtime, interrupts, panics and the progress
//! line.

use std::io::{self, Write as _};
use std::panic::{self, AssertUnwindSafe};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

use openxet_client::{ClientError, Progress};

use super::failure::client_error;
use super::report::human_bytes;
use crate::error::{CliError, ExitCode};

/// Set by the panic hook: some thread panicked, so the failure is a bug.
static PANICKED: AtomicBool = AtomicBool::new(false);

/// Runs `fut` on a multi-thread runtime, which the client core needs to
/// share rather than start its own pool (S10 "Context"), then ends the
/// progress line.
///
/// `SIGINT` or `SIGTERM` drops `fut`, which removes a partial download,
/// and fails with `interrupted` (exit 130). A panic is reported as
/// `internal`, never with its payload, which may hold request data.
pub(super) fn block_on<F: Future>(
    fut: F,
    progress: Option<&ProgressLine>,
) -> Result<F::Output, CliError> {
    install_panic_hook();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| {
            CliError::new(
                "runtime",
                format!("could not start the async runtime: {err}"),
                "Check the process's thread and file-descriptor limits.",
            )
        })?;
    let ran = panic::catch_unwind(AssertUnwindSafe(|| {
        runtime.block_on(async {
            tokio::select! {
                // First, so the handlers are installed before the transfer
                // starts.
                biased;
                signal = interrupted() => Err(signal),
                out = fut => Ok(out),
            }
        })
    }));
    if let Some(progress) = progress {
        progress.finish();
    }
    // Client-core tasks still running are abandoned, not awaited.
    runtime.shutdown_background();
    match ran {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(signal)) => Err(CliError::new(
            "interrupted",
            format!("interrupted by {signal}; nothing was registered or written"),
            "Run the command again; uploads skip data the server already has.",
        )
        .with_exit(ExitCode::INTERRUPTED)),
        Err(_) => Err(internal()),
    }
}

/// A client failure as the user sees it, or `internal` if it followed a
/// panic in the client core.
pub(super) fn failure(err: &ClientError) -> CliError {
    if PANICKED.load(Ordering::Relaxed) {
        internal()
    } else {
        client_error(err)
    }
}

fn internal() -> CliError {
    CliError::new(
        "internal",
        "openxet hit a bug (a panic) and stopped; nothing was registered or written",
        "This is a bug; please report it with the command you ran.",
    )
}

/// Records a panic and prints nothing: the default hook would print the
/// payload on stderr, raw even under `--json`.
fn install_panic_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        panic::set_hook(Box::new(|_| PANICKED.store(true, Ordering::Relaxed)));
    });
}

/// The first `SIGINT` or `SIGTERM` (Ctrl-C off Unix), by name.
async fn interrupted() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match (
            signal(SignalKind::interrupt()),
            signal(SignalKind::terminate()),
        ) {
            (Ok(mut int), Ok(mut term)) => tokio::select! {
                _ = int.recv() => "SIGINT",
                _ = term.recv() => "SIGTERM",
            },
            // Without handlers the default action stops the process.
            _ => std::future::pending().await,
        }
    }
    #[cfg(not(unix))]
    {
        match tokio::signal::ctrl_c().await {
            Ok(()) => "Ctrl-C",
            Err(_) => std::future::pending().await,
        }
    }
}

/// A progress line on stderr, redrawn in place.
pub(super) struct ProgressLine {
    verb: &'static str,
    drawn: AtomicBool,
}

impl ProgressLine {
    pub(super) fn new(verb: &'static str) -> Self {
        Self {
            verb,
            drawn: AtomicBool::new(false),
        }
    }

    pub(super) fn draw(&self, Progress { done, total }: Progress) {
        // Nothing is known yet: an empty line would only flicker.
        if total == 0 {
            return;
        }
        let mut stderr = io::stderr().lock();
        let _ = write!(
            stderr,
            "\r{} {} / {} ({}%)\x1b[K",
            self.verb,
            human_bytes(done),
            human_bytes(total),
            done.min(total) * 100 / total
        );
        let _ = stderr.flush();
        self.drawn.store(true, Ordering::Relaxed);
    }

    /// Ends the line, so what follows starts on its own.
    fn finish(&self) {
        if self.drawn.swap(false, Ordering::Relaxed) {
            let _ = writeln!(io::stderr());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // nextest runs each test in its own process, so the hook this installs
    // reaches no other test.
    #[test]
    fn a_panic_is_internal_and_its_payload_is_withheld() {
        let err = block_on(async { panic!("payload-marker") }, None)
            .map_or_else(|e| e, |()| unreachable!());
        assert_eq!(err.code, "internal");
        assert!(!err.message.contains("payload-marker"));
        assert!(PANICKED.load(Ordering::Relaxed));
        let later = failure(&ClientError::InvalidHash);
        assert_eq!(later.code, "internal");
    }
}
