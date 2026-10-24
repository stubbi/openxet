//! The server's log: text or JSON lines on stderr, and a panic hook that
//! logs through it.

use std::panic::PanicHookInfo;

use tracing::Level;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

use crate::config::LogFormat;

/// The crates logged at the configured level. Everything else (hyper,
/// sqlx, …) logs warnings and errors only.
const OWN_TARGETS: &[&str] = &[
    "openxet",
    "openxet_auth",
    "openxet_cas",
    "openxet_index",
    "openxet_storage",
    "tower_http",
];

fn filter(level: Level) -> Targets {
    OWN_TARGETS.iter().fold(
        Targets::new().with_default(Level::WARN),
        |targets, target| targets.with_target(*target, level),
    )
}

/// Held by every test that replaces the process-wide panic hook, so tests
/// sharing a process (as under `cargo test`) don't race over it.
#[cfg(test)]
pub static PANIC_HOOK_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Logs to stderr in `format` at `level`, and replaces the panic hook
/// with [`log_panic`].
pub fn init(format: LogFormat, level: Level) {
    let registry = tracing_subscriber::registry().with(filter(level));
    let fmt = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(false);
    match format {
        LogFormat::Text => registry.with(fmt).init(),
        LogFormat::Json => registry
            .with(fmt.json().with_current_span(true).with_span_list(false))
            .init(),
    }
    std::panic::set_hook(Box::new(log_panic));
}

/// Logs a panic's location at error level, inside the current span, so a
/// panic in a request handler carries the request's id.
///
/// The payload is never logged, and std's default hook, which prints it to
/// stderr, is replaced: a payload can hold request data or a credential.
pub fn log_panic(info: &PanicHookInfo<'_>) {
    let location = info.location().map_or_else(
        || "unknown".to_owned(),
        |l| format!("{}:{}:{}", l.file(), l.line(), l.column()),
    );
    tracing::error!(
        %location,
        "a thread panicked; the panic message is not logged, since it may hold request data"
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_panic_logs_its_location_and_request_id_but_never_its_payload() {
        let capture = Capture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_current_span(true)
            .with_writer(move || writer.clone())
            .finish();
        let _hook = PANIC_HOOK_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(log_panic));
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("request", request_id = "0192-test-id");
            let _entered = span.enter();
            let caught = std::panic::catch_unwind(|| panic!("oxt_payload-marker"));
            assert!(caught.is_err());
        });
        std::panic::set_hook(previous);

        let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("0192-test-id"), "{logs}");
        assert!(logs.contains(file!()), "{logs}");
        assert!(!logs.contains("payload-marker"), "{logs}");
    }

    #[test]
    fn other_crates_log_only_warnings() {
        let targets = filter(Level::TRACE);
        assert!(targets.would_enable("openxet_cas::xorbs", &Level::TRACE));
        assert!(targets.would_enable("tower_http::trace", &Level::DEBUG));
        assert!(!targets.would_enable("hyper::proto", &Level::INFO));
        assert!(targets.would_enable("sqlx::query", &Level::WARN));
    }
}
