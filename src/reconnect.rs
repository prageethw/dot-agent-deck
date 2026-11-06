//! The hydration gate: the ordering contract between the TUI's daemon event
//! subscription and its `ListAgents` hydration (fork issue #36).
//!
//! **This module holds ONLY the gate.** The subscriber itself is upstream's
//! [`crate::event_subscriber`]; the fork's gate is carried on top of it through
//! [`crate::event_subscriber::run_gated`]. Until the 2026-11-05 net merge this
//! file also held the fork's own subscriber loop (`run_event_subscriber`,
//! `resync_after_reconnect`, `apply_broadcast`); upstream's later loop (gap
//! resync #1520, protocol refusal #405, hook-binary notices #1637, the inbox
//! arms) superseded all of it, so it was deleted rather than re-ported. The name
//! `reconnect` is kept only to avoid churning every import.
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
//!    subsequent [`crate::event::BroadcastMsg`].
//!
//! Before the gate the two were unordered: the subscriber was spawned as a
//! detached task from `main`, and hydration ran concurrently on the blocking
//! TUI thread. Whichever connection the daemon happened to accept first won.
//! When the snapshot won, every event the daemon broadcast between the snapshot
//! and the subscription was delivered to nobody — the client had no receiver
//! registered yet, and nothing replays.
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
//! Upstream's `event_subscriber` does NOT close this window (verified for the
//! 2026-11-05 merge, `.dot-agent-deck/sync-merge-2026-11-05-subscriber-memo.md`
//! Q1): bootstrap hydration still makes its own `ListAgents` call that is not
//! ordered against the subscription, the first subscription reads only the
//! inboxes from `SubscribeEventsWithSnapshot`, and the monitor's idle side is
//! purely edge-triggered. That is why the gate is carried rather than dropped.
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
//! 3. Messages that arrive while hydration is still seeding cards are **buffered
//!    by the subscriber task**, not applied — `AppState::apply_event` drops any
//!    non-`SessionStart` event whose pane is not yet registered, so merely
//!    reordering the two connections would still lose the edge. Every
//!    [`crate::event::BroadcastMsg`] kind is held, not only `Event`, so the
//!    inbox and hook-binary-notice messages keep their arrival order.
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
//! # The same hazard later in the connection's life (issues #49 / #28)
//!
//! Everything above is about *bootstrap*. The identical loss happens whenever
//! the subscription dies mid-session — a daemon restart, a dead socket, or the
//! `KIND_STREAM_END "lagged"` tear-down. Upstream's loop re-reads the daemon's
//! agents on every resubscribe after the first (issue #1520, one atomic
//! `SubscribeEventsWithSnapshot`), which supersedes the fork's own
//! `resync_after_reconnect`. The gate only adds one rule on top: a resync whose
//! stream opened BEFORE hydration finished seeding is **held** until
//! [`HydrationGate::mark_seeded`], then applied snapshot-first with the buffered
//! events on top, so it cannot mint cards that hydration is about to create for
//! the same panes.
//!
//! # Every wait is bounded
//!
//! Hydration waits at most [`SUBSCRIBE_READY_TIMEOUT`] for the subscription, the
//! TUI calls [`HydrationGate::mark_seeded`] after its seeding loop even when
//! nothing was hydrated, and `main`'s 30 s watchdog calls it if `run_tui` never
//! does. A refused subscription (issue #405 protocol skew) releases the wait at
//! once through [`HydrationGate::release_waiters`] instead of timing out.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

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
    /// Raised by [`HydrationGate::release_waiters`]: the subscriber has given up
    /// for now, so nobody should keep waiting on `subscribed`. Deliberately NOT
    /// folded into `subscribed`, so [`HydrationGate::is_subscribed`] keeps
    /// meaning "a stream is actually up".
    released: watch::Sender<bool>,
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
                released: watch::Sender::new(false),
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

    /// Subscriber side: the subscription was REFUSED (issue #405 protocol skew),
    /// so no stream will come up until the daemon is replaced. Releases anyone
    /// in [`Self::wait_subscribed`] at once instead of leaving hydration to wait
    /// out [`SUBSCRIBE_READY_TIMEOUT`] before its own gated `ListAgents` is
    /// refused the same way. [`Self::wait_subscribed`] then returns `false`, and
    /// [`Self::is_subscribed`] stays `false`: this is "stop waiting", never
    /// "the stream is up". Idempotent; `send_replace` for the same reason as
    /// [`Self::mark_subscribed`]. A plain daemon-down failure does NOT call this,
    /// it keeps the wait-then-time-out behaviour.
    pub fn release_waiters(&self) {
        self.inner.released.send_replace(true);
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
    /// expiry or on [`Self::release_waiters`] — in which case the caller should
    /// proceed anyway rather than stall TUI startup, accepting the pre-fix
    /// window for this attach.
    pub async fn wait_subscribed(&self, timeout: Duration) -> bool {
        // Subscribe BEFORE reading the value. `subscribe()` marks the current
        // value as already seen, so a receiver created after the flip would
        // never see a `changed()` for it — checking first and subscribing after
        // would lose a signal that landed between the two.
        let mut rx = self.inner.subscribed.subscribe();
        let mut released = self.inner.released.subscribe();
        if *rx.borrow() {
            return true;
        }
        if *released.borrow() {
            return false;
        }
        tokio::time::timeout(timeout, async move {
            // `changed()` errs only when every Sender is dropped; we hold them
            // through `self`, so this loop cannot spin.
            loop {
                tokio::select! {
                    changed = rx.changed() => {
                        if changed.is_err() {
                            return false;
                        }
                        if *rx.borrow() {
                            return true;
                        }
                    }
                    changed = released.changed() => {
                        if changed.is_err() || *released.borrow() {
                            // Re-read `subscribed` once: a stream that came up
                            // in the same instant outranks the release.
                            return *rx.borrow();
                        }
                    }
                }
            }
        })
        .await
        .unwrap_or(false)
    }

    /// The subscriber's view of the `seeded` signal: it holds messages back
    /// until this reads `true`. `pub(crate)` for [`crate::event_subscriber`].
    pub(crate) fn seeded_receiver(&self) -> watch::Receiver<bool> {
        self.inner.seeded.subscribe()
    }
}

impl Default for HydrationGate {
    fn default() -> Self {
        Self::open()
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

    /// A refused subscription (issue #405) must stop hydration waiting out the
    /// full timeout, WITHOUT claiming a stream is up: `is_subscribed` keeps
    /// meaning "a stream is actually open".
    #[tokio::test]
    async fn release_waiters_ends_the_wait_without_claiming_a_stream() {
        let gate = HydrationGate::armed();
        let releaser = gate.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            releaser.release_waiters();
        });
        let started = std::time::Instant::now();
        assert!(!gate.wait_subscribed(Duration::from_secs(5)).await);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(!gate.is_subscribed());
        // Already released: a later wait returns at once.
        assert!(!gate.wait_subscribed(Duration::from_secs(5)).await);
        // A stream coming up afterwards still outranks the release.
        gate.mark_subscribed();
        assert!(gate.wait_subscribed(Duration::from_millis(20)).await);
    }
}
