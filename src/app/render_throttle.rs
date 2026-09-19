use super::*;

pub(super) const OUTPUT_MERGE_BYTE_CAP: usize = 64 * 1024;

/// Output parsed between UI-flush checkpoints during sustained traffic.
pub(super) const INGEST_FRAME_BUDGET: usize = 64 * 1024;

/// A busy or closing UI must never block a session pump indefinitely.
pub(super) const UI_FLUSH_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(50);

/// Do not deliberately pace a pump while a large unbounded-channel backlog is
/// already present. It catches up first, then paces the tail of the stream.
pub(super) const PACED_LOCAL_BACKLOG_LIMIT: usize = 1024 * 1024;
pub(super) const PACED_QUEUE_EVENT_LIMIT: usize = 256;

/// Max UI renders per second for a tab under sustained output (#209).
pub(super) const RENDER_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(33);
/// Echo produced shortly after a physical keypress should feel immediate. This
/// temporary 120 Hz ceiling is still coalesced, then falls back to 30 Hz once
/// the user stops typing so firehose output keeps its existing CPU protection.
pub(super) const INTERACTIVE_RENDER_MIN_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(8);
pub(super) const INTERACTIVE_ECHO_WINDOW: std::time::Duration =
    std::time::Duration::from_millis(180);
/// A scrolled-back content is content-anchored, so sustained output only
/// needs occasional model refreshes for its scrollbar metadata (#306).
pub(super) const SCROLLED_RENDER_MIN_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(100);

pub(super) fn term_buf(bufs: &TermBuffers, tab_id: &str) -> Option<TermBufferHandle> {
    bufs.lock().unwrap().get(tab_id).cloned()
}

pub(super) fn tab_render_interval(bufs: &TermBuffers, tab_id: &str) -> std::time::Duration {
    let Some(handle) = term_buf(bufs, tab_id) else {
        return RENDER_MIN_INTERVAL;
    };
    let interval = match handle.try_lock() {
        Ok(buf) if buf.view_offset > 0 => SCROLLED_RENDER_MIN_INTERVAL,
        Ok(buf) if std::time::Instant::now() < buf.interactive_echo_until => {
            INTERACTIVE_RENDER_MIN_INTERVAL
        }
        Ok(_) => RENDER_MIN_INTERVAL,
        // A busy ingest lock is itself a firehose signal. Deferring this
        // snapshot prevents the UI thread from joining the contention.
        Err(_) => SCROLLED_RENDER_MIN_INTERVAL,
    };
    interval
}

pub(super) fn with_term_buf<R>(
    bufs: &TermBuffers,
    tab_id: &str,
    f: impl FnOnce(&mut TermBuffer) -> R,
) -> Option<R> {
    let h = term_buf(bufs, tab_id)?;
    let mut guard = h.lock().unwrap();
    Some(f(&mut guard))
}

pub(super) fn ingest_terminal_output(bufs: &TermBuffers, tab_id: &str, chunk: &[u8]) -> Vec<u8> {
    if let Some(h) = term_buf(bufs, tab_id) {
        h.lock().unwrap().ingest(chunk)
    } else {
        Vec::new()
    }
}

pub(super) fn record_ingested_chunk(
    chunk_len: usize,
    ingested_since_checkpoint: &mut usize,
) -> bool {
    debug_assert!(*ingested_since_checkpoint < INGEST_FRAME_BUDGET);
    if chunk_len == 0 {
        return false;
    }

    let remaining = INGEST_FRAME_BUDGET - *ingested_since_checkpoint;
    if chunk_len < remaining {
        *ingested_since_checkpoint += chunk_len;
        false
    } else {
        *ingested_since_checkpoint = (chunk_len - remaining) % INGEST_FRAME_BUDGET;
        true
    }
}

pub(super) fn event_requires_immediate_ui(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::Connected
            | SessionEvent::Closed(_)
            | SessionEvent::HostKeyPrompt { .. }
            | SessionEvent::CredentialPrompt { .. }
    )
}

pub(super) struct TabRenderTicket {
    gate: Arc<TabRenderGate>,
    generation: u64,
}

pub(super) fn register_tab_render_request(
    tab_id: &str,
    gates: &RenderGates,
) -> Option<(Arc<TabRenderGate>, TabRenderTicket, bool)> {
    let gate = {
        let map = gates.lock().unwrap();
        map.get(tab_id).cloned()
    }?;
    let (generation, should_schedule) = gate.request()?;
    let ticket = TabRenderTicket {
        gate: gate.clone(),
        generation,
    };
    Some((gate, ticket, should_schedule))
}

pub(super) fn request_tab_render(
    weak: slint::Weak<AppWindow>,
    tab_id: &str,
    bufs: &TermBuffers,
    gates: &RenderGates,
) -> Option<TabRenderTicket> {
    let (gate, ticket, should_schedule) = register_tab_render_request(tab_id, gates)?;
    if !should_schedule {
        return Some(ticket);
    }

    let weak2 = weak.clone();
    let tid = tab_id.to_string();
    let bufs2 = bufs.clone();
    let gate2 = gate.clone();
    // Always bounce through the event loop from pump / worker threads.
    // Never call invoke_from_event_loop from inside a UI callback — that
    // deadlocks Slint (opening a second tab then froze the whole app).
    if slint::invoke_from_event_loop(move || {
        run_coalesced_tab_render(&weak2, &tid, &bufs2, gate2);
    })
    .is_err()
    {
        // The event loop is gone. Wake any pump waiting on this ticket and
        // reject future requests instead of leaving the gate scheduled forever.
        gate.close();
    }
    Some(ticket)
}

/// UI-thread variant for synthetic Output events. It shares the same gate but
/// enters the throttle directly because invoking Slint from its own callback
/// can deadlock.
pub(super) fn request_tab_render_from_ui(
    weak: slint::Weak<AppWindow>,
    tab_id: &str,
    bufs: &TermBuffers,
    gates: &RenderGates,
) {
    let Some((gate, _, should_schedule)) = register_tab_render_request(tab_id, gates) else {
        return;
    };
    if should_schedule {
        run_coalesced_tab_render(&weak, tab_id, bufs, gate);
    }
}

pub(super) fn wait_for_ui_flush(ticket: Option<TabRenderTicket>) {
    if let Some(ticket) = ticket {
        let _ = ticket
            .gate
            .wait_for(ticket.generation, UI_FLUSH_ACK_TIMEOUT);
    }
}

/// UI-thread entry: honour the throttle, then render. Timer must be created
/// here — not on pump threads (#209).
pub(super) fn run_coalesced_tab_render(
    weak: &slint::Weak<AppWindow>,
    tab_id: &str,
    bufs: &TermBuffers,
    gate: Arc<TabRenderGate>,
) {
    let delay = gate.flush_delay(tab_render_interval(bufs, tab_id));

    let weak2 = weak.clone();
    let tid = tab_id.to_string();
    let bufs2 = bufs.clone();

    if delay.is_zero() {
        do_tab_render_flush(&weak2, &tid, &bufs2, gate);
    } else {
        slint::Timer::single_shot(delay, move || {
            do_tab_render_flush(&weak2, &tid, &bufs2, gate);
        });
    }
}

/// UI-thread only: commit the vt100 snapshot to Slint's model, then reschedule
/// if output arrived after this snapshot began. `request_redraw` is asynchronous,
/// so completion acknowledges a model flush rather than GPU presentation.
pub(super) fn do_tab_render_flush(
    weak: &slint::Weak<AppWindow>,
    tab_id: &str,
    bufs: &TermBuffers,
    gate: Arc<TabRenderGate>,
) {
    let Some(through) = gate.begin_flush() else {
        return;
    };

    let visible = if let Some(win) = weak.upgrade() {
        if visible_tab_ids(&win).contains(tab_id) {
            rebuild_tab_display(&win, bufs, tab_id);
            true
        } else {
            false
        }
    } else {
        false
    };

    if gate.finish_flush(through, visible) {
        let weak2 = weak.clone();
        let tid = tab_id.to_string();
        let bufs2 = bufs.clone();
        // Defer the continuation to avoid recursive flushes for hidden tabs,
        // whose last-visible timestamp intentionally does not throttle them.
        slint::Timer::single_shot(std::time::Duration::ZERO, move || {
            run_coalesced_tab_render(&weak2, &tid, &bufs2, gate);
        });
    }
}
