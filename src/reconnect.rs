//! TUI-side reconnect bootstrap: the daemon event subscription and the
//! ordering contract it has with `ListAgents` hydration (fork issue #36).
//!
//! # The window this module exists to close
//!
//! On attach/reconnect the TUI rebuilds its view of the world from two
//! independent daemon connections:
//!
//! 1. a **snapshot** — `ListAgents`, whose `AgentRecord.live`
//!    [`crate::state::SessionSnapshot`] carries each session's status at the
//!    instant the daemon serialized it; and
//! 2. a **stream** — a long-lived `SubscribeEvents` connection carrying every
//!    subsequent [`BroadcastMsg`].
//!
//! Before this module the two were unordered: `spawn_event_subscriber` was
//! spawned as a detached task from `main`, and hydration ran concurrently on
//! the blocking TUI thread. Whichever connection the daemon happened to accept
//! first won. When the snapshot won, every event the daemon broadcast between
//! the snapshot and the subscription was delivered to nobody — the client had
//! no receiver registered yet, and nothing replays.
//!
//! That is invisible for most events (the next one corrects the card), but it
//! is permanent for an **edge**. The daemon's shell-activity monitor
//! (`run_shell_activity_monitor`) is edge-triggered: it emits exactly one
//! `ShellBusy` when a foreground shell command starts and exactly one
//! `ShellIdle` when it ends. If the snapshot captures the synthetic `Working`
//! and the paired `ShellIdle` falls in the gap, hydration seeds `Working` from
//! a snapshot that is already stale and no further event ever arrives to
//! correct it. The pane reads `Working` for the rest of the session.
//!
//! This is distinct from fork issue #21 (fixed in PR #34): there the
//! `ShellIdle` *was* delivered, but hydration dropped the
//! `shell_synthetic_working` provenance marker so `apply_event` declined to
//! revert. Here the event is never delivered at all.
//!
//! # The fix: subscribe first, buffer across hydration
//!
//! The daemon registers the broadcast receiver **before** it writes the
//! subscription's OK `RESP` (see `handle_subscribe_events` in
//! [`crate::daemon_protocol`]), so a client that has read that RESP is
//! guaranteed to receive every event broadcast afterwards. That makes the gap
//! closable entirely client-side, with no wire change:
//!
//! 1. The subscriber establishes the stream and calls
//!    [`HydrationGate::mark_subscribed`].
//! 2. Hydration waits for that signal ([`HydrationGate::wait_subscribed`])
//!    before issuing `ListAgents`, so the snapshot is strictly newer than the
//!    subscription.
//! 3. Events that arrive while hydration is still seeding cards are **buffered
//!    by the subscriber task**, not applied — `AppState::apply_event` drops any
//!    non-`SessionStart` event whose pane is not yet registered, so merely
//!    reordering the two connections would still lose the edge.
//! 4. Once hydration has registered and seeded every pane it calls
//!    [`HydrationGate::mark_seeded`]; the subscriber drains its buffer in
//!    arrival order and then applies live.
//!
//! Replaying buffered events on top of the snapshot is safe: they are applied
//! in the order the daemon broadcast them, and the daemon applies each event to
//! its own `AppState` under the same write-lock acquisition it broadcasts under
//! (`ingest_event`, PRD #386), so the snapshot can never be captured *between*
//! an event's broadcast and its application. A replayed event the snapshot
//! already reflects is therefore a redundant re-application of the same
//! transition, never a regression to an older one.
//!
//! # What this does NOT close
//!
//! Only the *bootstrap* gap. If the subscription later dies (daemon restart, a
//! `KIND_STREAM_END "lagged"` tear-down) the reconnect loop below re-subscribes
//! but the TUI does not re-hydrate, so events broadcast during that outage are
//! still lost. That is pre-existing behaviour and out of scope for issue #36.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use crate::daemon_client::DaemonClient;
use crate::event::BroadcastMsg;
use crate::state::SharedState;

/// How long hydration will wait for the event subscription to be confirmed
/// before giving up and issuing `ListAgents` anyway.
///
/// Sized against [`crate::daemon_client::DaemonClient::subscribe_events`],
/// which is one connect plus one REQ/RESP round-trip against a local Unix
/// socket — sub-millisecond on a healthy daemon. Five seconds matches
/// `embedded_pane`'s `HYDRATE_LIST_TIMEOUT`, the bound on the very next call
/// hydration makes.
///
/// Expiry is a **degradation, never a hang**: hydration proceeds with the
/// window open (exactly today's behaviour) rather than blocking TUI startup on
/// a daemon that is not answering.
pub const SUBSCRIBE_READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Upper bound on events held back while hydration seeds its cards.
///
/// The buffer spans one `ListAgents` round-trip plus the per-agent attaches —
/// well under a second in practice, during which a normal deck produces a
/// handful of events. The cap exists so a pathological daemon flooding the
/// broadcast channel while hydration is stalled cannot grow client memory
/// without limit. On overflow the OLDEST buffered event is dropped: the
/// buffered window's purpose is to land the deck on the daemon's *current*
/// state, and the newest events are the ones that carry it.
pub const HYDRATION_BUFFER_CAP: usize = 1024;

/// Coordination handle between the event subscriber and reconnect hydration.
///
/// Cheap to clone — every clone refers to the same pair of signals. See the
/// module docs for the ordering contract; the two signals are:
///
/// * **subscribed** — raised by the subscriber once the daemon has confirmed
///   the `SubscribeEvents` stream. Hydration waits for it before capturing the
///   `ListAgents` snapshot.
/// * **seeded** — raised by hydration once every hydrated pane is registered
///   and seeded. The subscriber holds events back until it is raised, then
///   drains them in arrival order.
#[derive(Clone, Debug)]
pub struct HydrationGate {
    inner: Arc<GateInner>,
}

#[derive(Debug)]
struct GateInner {
    subscribed: watch::Sender<bool>,
    seeded: watch::Sender<bool>,
}

impl HydrationGate {
    /// A gate that is armed: events are buffered until [`Self::mark_seeded`],
    /// and [`Self::wait_subscribed`] blocks until [`Self::mark_subscribed`].
    /// This is what the TUI's reconnect bootstrap uses.
    pub fn armed() -> Self {
        Self {
            inner: Arc::new(GateInner {
                subscribed: watch::Sender::new(false),
                seeded: watch::Sender::new(false),
            }),
        }
    }

    /// A gate that is already open in both directions — no hydration will run
    /// against it, so events apply the moment they arrive and nothing ever
    /// waits. The right choice for any caller that subscribes without
    /// rehydrating.
    pub fn open() -> Self {
        let gate = Self::armed();
        gate.mark_subscribed();
        gate.mark_seeded();
        gate
    }

    /// Subscriber side: the daemon has confirmed the event stream. Idempotent —
    /// the reconnect loop calls it again after every successful re-subscribe.
    ///
    /// `send_replace`, NOT `send`: `watch::Sender::send` FAILS — and leaves the
    /// value untouched — when no receiver exists yet, and that is the normal
    /// case here. The subscriber typically confirms the stream before hydration
    /// has called [`Self::wait_subscribed`] (which is what creates the first
    /// receiver), so a plain `send` would silently drop the signal and leave
    /// hydration waiting out its full timeout on a stream that was already up.
    pub fn mark_subscribed(&self) {
        self.inner.subscribed.send_replace(true);
    }

    /// Hydration side: every hydrated pane has been registered and seeded, so
    /// buffered events can now land on real cards. Idempotent. `send_replace`
    /// for the same reason as [`Self::mark_subscribed`].
    pub fn mark_seeded(&self) {
        self.inner.seeded.send_replace(true);
    }

    /// True once [`Self::mark_seeded`] has been called.
    pub fn is_seeded(&self) -> bool {
        *self.inner.seeded.borrow()
    }

    /// True once [`Self::mark_subscribed`] has been called.
    pub fn is_subscribed(&self) -> bool {
        *self.inner.subscribed.borrow()
    }

    /// Hydration side: wait for the subscription to be confirmed, bounded by
    /// `timeout`. Returns `true` when the stream is up (the snapshot that
    /// follows is then strictly newer than the subscription) and `false` on
    /// expiry — in which case the caller should proceed anyway rather than
    /// stall TUI startup, accepting the pre-fix window for this attach.
    pub async fn wait_subscribed(&self, timeout: Duration) -> bool {
        // Subscribe BEFORE reading the value. `subscribe()` marks the current
        // value as already seen, so a receiver created after the flip would
        // never see a `changed()` for it — checking first and subscribing after
        // would lose a signal that landed between the two.
        let mut rx = self.inner.subscribed.subscribe();
        if *rx.borrow() {
            return true;
        }
        tokio::time::timeout(timeout, async move {
            // `changed()` errs only when every Sender is dropped; we hold one
            // through `self`, so this loop cannot spin.
            while rx.changed().await.is_ok() {
                if *rx.borrow() {
                    return;
                }
            }
        })
        .await
        .is_ok()
    }

    fn seeded_receiver(&self) -> watch::Receiver<bool> {
        self.inner.seeded.subscribe()
    }
}

impl Default for HydrationGate {
    fn default() -> Self {
        Self::open()
    }
}

/// PRD #76 M2.17 (hook events) / M2.19 (delegate signals): open a long-lived
/// `SubscribeEvents` connection against the daemon and route each
/// [`BroadcastMsg::Event`] into the TUI's `AppState` via `apply_event`.
///
/// PRD #93 round-5: the delegate / work-done variants used to ride this channel
/// too — the daemon couldn't dispatch them locally and the TUI re-ran the
/// role-validation guards. The daemon now owns dispatch end to end (writes the
/// prompt directly into the target pane's PTY), so only hook events flow
/// through here.
///
/// Reconnects with a small backoff on transport errors so a daemon restart or a
/// `KIND_STREAM_END "lagged"` tear-down recovers automatically.
///
/// Fork issue #36: `gate` orders this subscription against reconnect
/// hydration. Pass [`HydrationGate::armed`] when a `hydrate_from_daemon` pass
/// will follow (the TUI bootstrap), or [`HydrationGate::open`] when it will
/// not. See the module docs for why buffering — not merely reordering — is
/// what closes the window.
pub fn spawn_event_subscriber(attach_path: PathBuf, state: SharedState, gate: HydrationGate) {
    tokio::spawn(async move {
        run_event_subscriber(attach_path, state, gate).await;
    });
}

/// The body of [`spawn_event_subscriber`], as an ordinary future so tests can
/// drive it with their own task/abort handle. Never returns on its own.
pub async fn run_event_subscriber(attach_path: PathBuf, state: SharedState, gate: HydrationGate) {
    // Backoff parameters tuned for "daemon briefly unavailable" rather than
    // long outages: a fresh-daemon ready window is sub-second, so a 500ms
    // initial delay catches most transient cases, and we cap at 5s so a stuck
    // daemon doesn't burn CPU on reconnect attempts.
    let mut delay = Duration::from_millis(500);
    let max_delay = Duration::from_secs(5);
    let client = DaemonClient::new(attach_path);
    loop {
        match client.subscribe_events().await {
            Ok(mut sub) => {
                // Reset backoff on a successful subscribe.
                delay = Duration::from_millis(500);
                // Fork issue #36: the daemon registered our broadcast receiver
                // BEFORE it wrote the OK RESP we just read, so from here on no
                // event can be missed. Releasing hydration only now is what
                // makes the `ListAgents` snapshot strictly newer than this
                // subscription.
                gate.mark_subscribed();

                // Events received before hydration finished seeding its cards.
                // Held here rather than applied because `apply_event` drops any
                // non-`SessionStart` event whose pane is not yet registered —
                // applying early would lose the edge just as surely as never
                // receiving it.
                let mut pending: VecDeque<BroadcastMsg> = VecDeque::new();
                let mut seeded_rx = gate.seeded_receiver();
                loop {
                    tokio::select! {
                        recv = sub.next_event() => {
                            match recv {
                                Ok(Some(msg)) => {
                                    if *seeded_rx.borrow() {
                                        // Drain first: `select!` may pick this
                                        // branch on the same poll the gate
                                        // opened, and buffered events must
                                        // still land in arrival order.
                                        drain_pending(&state, &mut pending).await;
                                        apply_broadcast(&state, msg).await;
                                    } else {
                                        if pending.len() >= HYDRATION_BUFFER_CAP {
                                            tracing::warn!(
                                                cap = HYDRATION_BUFFER_CAP,
                                                "subscribe_events: hydration buffer full, dropping oldest \
                                                 held event"
                                            );
                                            pending.pop_front();
                                        }
                                        pending.push_back(msg);
                                    }
                                }
                                Ok(None) => break,
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        "subscribe_events: stream error, reconnecting"
                                    );
                                    break;
                                }
                            }
                        }
                        // Hydration finished while the stream was quiet. Without
                        // this branch the held events would sit until the next
                        // event happened to arrive — which, for the edge this
                        // whole module exists to preserve, may be never.
                        changed = seeded_rx.changed() => {
                            if changed.is_err() {
                                // Every gate Sender dropped. Nothing will ever
                                // open it, so stop holding events back.
                                drain_pending(&state, &mut pending).await;
                                break;
                            }
                            if *seeded_rx.borrow() {
                                drain_pending(&state, &mut pending).await;
                            }
                        }
                    }
                }
                // The stream is going away; don't strand anything we held.
                // Applying now is a no-op for panes hydration never registered
                // and correct for the ones it did.
                drain_pending(&state, &mut pending).await;
            }
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "subscribe_events: subscribe failed, retrying"
                );
            }
        }
        tokio::time::sleep(delay).await;
        delay = std::cmp::min(delay * 2, max_delay);
    }
}

async fn drain_pending(state: &SharedState, pending: &mut VecDeque<BroadcastMsg>) {
    while let Some(msg) = pending.pop_front() {
        apply_broadcast(state, msg).await;
    }
}

async fn apply_broadcast(state: &SharedState, msg: BroadcastMsg) {
    match msg {
        BroadcastMsg::Event(event) => state.write().await.apply_event(event),
        // PRD #120: a daemon-spawned orchestration (issue dispatch). Queue it
        // for the render loop, which owns the TabManager + pane controller and
        // builds the live tab. The subscriber task can't touch those.
        BroadcastMsg::OrchestrationSurface(surface) => {
            state.write().await.queue_orchestration_surface(surface)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn open_gate_never_blocks_and_never_buffers() {
        let gate = HydrationGate::open();
        assert!(gate.is_subscribed());
        assert!(gate.is_seeded());
        assert!(gate.wait_subscribed(Duration::from_millis(50)).await);
    }

    #[tokio::test]
    async fn armed_gate_wait_subscribed_times_out_without_a_subscriber() {
        let gate = HydrationGate::armed();
        assert!(!gate.is_subscribed());
        assert!(!gate.wait_subscribed(Duration::from_millis(20)).await);
    }

    /// The production ordering: the subscriber confirms the stream BEFORE
    /// hydration ever waits on it, so the signal has to survive being raised
    /// with no receiver in existence. A plain `watch::Sender::send` does not —
    /// it errs and leaves the value `false` — which would have left hydration
    /// waiting out its full timeout on a stream that was already up.
    #[tokio::test]
    async fn mark_subscribed_survives_being_raised_before_anyone_waits() {
        let gate = HydrationGate::armed();
        gate.mark_subscribed();
        assert!(gate.is_subscribed());
        assert!(gate.wait_subscribed(Duration::from_millis(50)).await);
    }

    #[tokio::test]
    async fn armed_gate_wait_subscribed_returns_when_the_subscriber_signals() {
        let gate = HydrationGate::armed();
        let signaller = gate.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            signaller.mark_subscribed();
        });
        assert!(gate.wait_subscribed(Duration::from_secs(2)).await);
        assert!(gate.is_subscribed());
    }
}
