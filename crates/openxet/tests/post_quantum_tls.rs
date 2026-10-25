//! Every TLS client in the binary offers the hybrid post-quantum key
//! exchange `X25519MLKEM768` first (ADR 0011 item 1, issue #66), so a
//! dependency update that drops rustls's `prefer-post-quantum` fails here.
//!
//! Each test runs the real binary against a listener that reads the first
//! `ClientHello` and checks its `supported_groups`, in the client's order
//! of preference (RFC 8446 §4.2.7). The handshake is then dropped; the
//! client's failure is expected and ignored.

// The helpers are test code too; a panic is the intended failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use rustls::NamedGroup;
use rustls::server::Acceptor;

/// A token the listener never checks, passed in the environment.
const TOKEN: &str = "oxt_not-a-configured-token";

/// `openxet` with no inherited openxet, Xet or proxy configuration.
fn openxet(endpoint: &str) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_openxet"));
    for (name, _) in std::env::vars_os() {
        let name_str = name.to_string_lossy();
        if name_str.starts_with("OPENXET_")
            || name_str.starts_with("HF_")
            || name_str.to_ascii_lowercase().ends_with("_proxy")
        {
            cmd.env_remove(name);
        }
    }
    cmd.env("OPENXET_ENDPOINT", endpoint)
        .env("OPENXET_TOKEN", TOKEN)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd
}

/// The groups of the first `ClientHello` that `start`'s process sends to
/// an `https` endpoint, in the order it offers them.
fn offered_groups(start: impl FnOnce(&str) -> Child) -> Vec<NamedGroup> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("https://{}", listener.local_addr().unwrap());
    // The client core checks that the endpoint accepts a TCP connection
    // before its first request (`crates/client/src/net.rs`), so a
    // connection closed before any byte is skipped.
    let hello = thread::spawn(move || {
        'connections: loop {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_mins(2)))
                .unwrap();
            let mut acceptor = Acceptor::default();
            let mut read_any = false;
            loop {
                match acceptor.read_tls(&mut stream) {
                    Ok(0) | Err(_) if !read_any => continue 'connections,
                    Ok(0) => panic!("the client closed inside its ClientHello"),
                    read => read.unwrap(),
                };
                read_any = true;
                if let Some(accepted) = acceptor.accept().map_err(|(e, _)| e).unwrap() {
                    return accepted.client_hello().named_groups().unwrap().to_vec();
                }
            }
        }
    });
    let mut child = start(&endpoint);
    let groups = hello.join();
    let _ = child.kill();
    let _ = child.wait();
    groups.unwrap()
}

fn assert_hybrid_first(groups: &[NamedGroup]) {
    assert_eq!(
        groups.first(),
        Some(&NamedGroup::X25519MLKEM768),
        "offered groups: {groups:?}"
    );
    assert!(
        groups.contains(&NamedGroup::X25519),
        "X25519 stays offered for servers without the hybrid: {groups:?}"
    );
}

#[test]
fn the_client_core_offers_the_hybrid_key_exchange_first() {
    let dir = tempfile::tempdir().unwrap();
    let groups = offered_groups(|endpoint| {
        openxet(endpoint)
            .args(["download", &"0".repeat(64), "--size", "1", "--output"])
            .arg(dir.path().join("out"))
            .arg("--cache-dir")
            .arg(dir.path().join("cache"))
            .spawn()
            .unwrap()
    });
    assert_hybrid_first(&groups);
}

#[test]
fn label_requests_offer_the_hybrid_key_exchange_first() {
    let groups = offered_groups(|endpoint| {
        openxet(endpoint)
            .args(["label", "list", "alice/my-model"])
            .spawn()
            .unwrap()
    });
    assert_hybrid_first(&groups);
}
