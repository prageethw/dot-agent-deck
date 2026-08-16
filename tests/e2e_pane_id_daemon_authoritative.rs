#![cfg(feature = "e2e")]

//! L2 coverage for PRD #365 M2 — the daemon, not the client, mints
//! `pane_id` on the `StartAgent` path.
//!
//! Drives a real (non-TUI) `dot-agent-deck daemon serve` subprocess directly
//! over its attach protocol, the same "synthetic `StartAgent` over the
//! daemon protocol" shape `dashboard/pane/001` already documents. The
//! response is decoded as raw JSON rather than through the strongly-typed
//! `AttachResponse` on purpose: this test's whole point is a field
//! (`pane_id`) that struct does not carry yet, and decoding through it would
//! turn a missing field into a compile error instead of the runtime
//! assertion this test exists to pin.

mod common;

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use common::spawn_daemon_serve;
use dot_agent_deck::agent_pty::DOT_AGENT_DECK_PANE_ID;
use dot_agent_deck::daemon_protocol::{AttachRequest, KIND_REQ, KIND_RESP};
use spec::spec;

/// Send `req` over `socket` using the same wire framing as
/// `common::attach_request_on`, but decode the reply as raw JSON. See the
/// module doc comment for why: `AttachResponse` has no `pane_id` field yet,
/// so decoding through the typed struct would make this test fail to
/// compile rather than fail its assertions for the right reason.
fn start_agent_raw_response(socket: &Path, req: &AttachRequest) -> serde_json::Value {
    let mut stream = UnixStream::connect(socket).expect("connect to daemon attach socket");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();

    let payload = serde_json::to_vec(req).expect("serialize AttachRequest");
    let mut header = [0u8; 5];
    header[0] = KIND_REQ;
    header[1..5].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    stream.write_all(&header).expect("write request header");
    stream.write_all(&payload).expect("write request payload");
    stream.flush().expect("flush request");

    let mut resp_header = [0u8; 5];
    stream
        .read_exact(&mut resp_header)
        .expect("read response header");
    assert_eq!(
        resp_header[0], KIND_RESP,
        "expected a RESP frame, got kind 0x{:02x}",
        resp_header[0]
    );
    let len = u32::from_be_bytes([
        resp_header[1],
        resp_header[2],
        resp_header[3],
        resp_header[4],
    ]) as usize;
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).expect("read response body");
    serde_json::from_slice(&body).expect("decode response JSON")
}

/// A `StartAgent` request for a `cat` stub (no real agent needed), proposing
/// `client_pane_id` as its own `DOT_AGENT_DECK_PANE_ID` — the pre-M2 shape,
/// which this test proves the daemon must stop trusting.
fn start_agent_request(client_pane_id: &str) -> AttachRequest {
    AttachRequest::StartAgent {
        command: Some("cat".into()),
        cwd: None,
        rows: 24,
        cols: 80,
        env: vec![(DOT_AGENT_DECK_PANE_ID.into(), client_pane_id.into())],
        display_name: None,
        tab_membership: None,
        agent_type: None,
        seed: None,
    }
}

/// Scenario: Start a real `dot-agent-deck daemon serve` subprocess and drive
/// its attach protocol directly (no TUI): send two `StartAgent` requests,
/// each spawning a `cat` stub and each proposing its own
/// `DOT_AGENT_DECK_PANE_ID` value in `env`. Assert both raw JSON responses
/// carry a present, mutually-distinct, daemon-minted `pane_id` that is
/// neither call's client-proposed value — proving the daemon, not the
/// client, is authoritative for pane identity.
#[spec("daemon/pane-id/001")]
#[test]
fn pane_id_001_start_agent_mints_distinct_daemon_pane_ids() {
    let daemon = spawn_daemon_serve(None, "0");

    let resp_a = start_agent_raw_response(
        &daemon.attach_socket,
        &start_agent_request("client-chosen-alpha"),
    );
    assert!(
        resp_a["ok"].as_bool().unwrap_or(false),
        "first StartAgent call failed: {resp_a}"
    );
    let pane_id_a = resp_a.get("pane_id").and_then(|v| v.as_str());
    assert!(
        pane_id_a.is_some(),
        "AttachResponse carried no daemon-minted `pane_id` for the first \
         StartAgent call — PRD #365 M2 daemon-side minting is not \
         implemented yet. Full response: {resp_a}"
    );

    let resp_b = start_agent_raw_response(
        &daemon.attach_socket,
        &start_agent_request("client-chosen-beta"),
    );
    assert!(
        resp_b["ok"].as_bool().unwrap_or(false),
        "second StartAgent call failed: {resp_b}"
    );
    let pane_id_b = resp_b.get("pane_id").and_then(|v| v.as_str());
    assert!(
        pane_id_b.is_some(),
        "AttachResponse carried no daemon-minted `pane_id` for the second \
         StartAgent call — PRD #365 M2 daemon-side minting is not \
         implemented yet. Full response: {resp_b}"
    );

    let pane_id_a = pane_id_a.unwrap();
    let pane_id_b = pane_id_b.unwrap();

    assert_ne!(
        pane_id_a, pane_id_b,
        "two independent StartAgent calls must mint two DISTINCT daemon \
         pane_ids, got the same value {pane_id_a:?} twice"
    );
    assert_ne!(
        pane_id_a, "client-chosen-alpha",
        "the daemon must not simply echo back the client-proposed \
         DOT_AGENT_DECK_PANE_ID; it must mint its own — got the client's \
         own value back unchanged"
    );
    assert_ne!(
        pane_id_b, "client-chosen-beta",
        "the daemon must not simply echo back the client-proposed \
         DOT_AGENT_DECK_PANE_ID; it must mint its own — got the client's \
         own value back unchanged"
    );
}
