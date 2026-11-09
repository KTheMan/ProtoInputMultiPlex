// Called only on the disposable hosted runner after exact GUI receiver approval.
// The Rust API parses raw JSON integers; never round epoch/revision through Number.
export async function runHostedModeAcceptance(client) {
  const initial = client.state_json();
  const ready = JSON.parse(initial);
  const diagnostic = window.pimHostedDiagnostic = {
    operation: 'initial', awaiting_reply: false, response_received: false,
    reply_consumed: false, terminal_seen: false, terminal_reason: 'none',
    frame_budget_exceeded: false, state_read_failed: false,
    last_hosted_phase: 'none', last_hosted_mode: 'none', last_minimized: null
  };
  function observeHosted(hosted) {
    if (!hosted) return;
    diagnostic.last_hosted_phase = ['native','starting','ready','stopping','assigned','quarantined','unavailable'].includes(hosted.phase) ? hosted.phase : 'other';
    diagnostic.last_hosted_mode = ['native','switched','concurrent'].includes(hosted.mode) ? hosted.mode : 'other';
    diagnostic.last_minimized = typeof hosted.minimized === 'boolean' ? hosted.minimized : null;
  }
  function observeState(state) {
    diagnostic.live = state.stream_state === 'Live';
    diagnostic.disconnected = state.stream_state === 'Disconnected';
    diagnostic.needs_snapshot = state.stream_state === 'NeedsSnapshot';
    diagnostic.hosted_pending = typeof state.hosted_request_pending === 'boolean' ? state.hosted_request_pending : null;
    diagnostic.subscription_pending = typeof state.subscription_pending === 'boolean' ? state.subscription_pending : null;
    diagnostic.hosted_present = !!state.hosted;
    observeHosted(state.hosted);
  }
  function begin(operation) {
    diagnostic.operation = operation;
    diagnostic.awaiting_reply = false;
    diagnostic.response_received = false;
    diagnostic.reply_consumed = false;
  }
  observeState(ready);
  if (ready.stream_state !== 'Live' || !ready.event_grant?.host_state ||
      !ready.control_grant?.host_mode_control || !ready.control_grant?.host_viewport_control ||
      ready.event_grant?.pointer_events || ready.event_grant?.key_events ||
      ready.control_grant?.pointer_input || ready.control_grant?.key_input) {
    throw new Error('FAIL: host-only receiver authority');
  }
  const epoch = initial.match(/"epoch"\s*:\s*([0-9]+)/)?.[1];
  if (!epoch || !ready.hosted || ready.hosted.phase !== 'native') {
    throw new Error('FAIL: initial native observation');
  }
  const frames = [];
  const terminalReasons = {
    'Hosted request deadline reached; action outcome unconfirmed; reconnect and inspect without replay': 'hosted_deadline',
    'Hosted receipt conflicts with current stream': 'receipt_stream_conflict',
    'Hosted receipt does not match pending request; outcome unconfirmed': 'receipt_correlation',
    'Hosted action outcome uncertain; reconnect and inspect without replay': 'action_uncertain',
    'Hosted event conflicts with prior receipt': 'event_receipt_conflict',
    'Unexpected response identifier': 'response_id',
    'Response version mismatch': 'response_version',
    'Invalid ordered event stream; reconnect explicitly': 'ordered_stream',
    'Fresh snapshot validation failed': 'snapshot_invalid',
    'Invalid typed event frame': 'frame_invalid',
    'Event connection closed; reconnect explicitly': 'socket_closed',
    'Event connection failed; verify endpoint, exact Origin and event credential': 'socket_error',
    'Event authority ended; reconnect explicitly after host approval': 'authority_ended',
    'Request rejected (StaleRevision); reconnect explicitly': 'stale_revision',
    'Request rejected (Denied); reconnect explicitly': 'denied',
    'Request rejected (Limit); reconnect explicitly': 'request_limit',
    'Request rejected (Ended); reconnect explicitly': 'authority_ended'
  };
  client.set_event_handler((raw) => {
    if (frames.length >= 128) {
      diagnostic.frame_budget_exceeded = true;
      throw new Error('FAIL: event frame budget');
    }
    const frame = JSON.parse(raw);
    if (frame.kind === 'response') diagnostic.response_received = true;
    if (frame.kind === 'response' && frame.reply?.status === 'ok') {
      observeHosted(frame.reply.response?.state || frame.reply.response?.snapshot?.hosted);
    }
    if (frame.kind === 'event_stream' && frame.stream?.kind === 'events') {
      for (const envelope of frame.stream.events) {
        if (envelope.event?.kind === 'hosted_state_changed') observeHosted(envelope.event.state);
      }
    }
    if (frame.kind === 'local_disconnected') {
      diagnostic.terminal_seen = true;
      diagnostic.terminal_reason = typeof frame.message === 'string' &&
        Object.prototype.hasOwnProperty.call(terminalReasons, frame.message) ?
        terminalReasons[frame.message] : 'unrecognized';
    }
    Object.defineProperty(frame, '__raw', {value: raw});
    frames.push(frame);
  });
  async function reply(kind, timeoutMs = 65000) {
    diagnostic.awaiting_reply = true;
    const until = Date.now() + timeoutMs;
    for (;;) {
      // A fenced client cannot complete this action without forbidden reconnect/replay.
      if (diagnostic.terminal_seen) throw new Error('FAIL: terminal before hosted receipt');
      const index = frames.findIndex((frame) => frame.kind === 'response');
      if (index >= 0) {
        const frame = frames.splice(index, 1)[0];
        diagnostic.awaiting_reply = false;
        diagnostic.reply_consumed = true;
        if (frame.reply?.status !== 'ok' || frame.reply.response?.kind !== kind) {
          throw new Error('FAIL: wrong or uncertain host receipt');
        }
        const response = frame.reply.response;
        Object.defineProperty(response, '__raw', {value: frame.__raw});
        return response;
      }
      if (Date.now() >= until) throw new Error('FAIL: hosted receipt deadline');
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  }
  async function live(phase, mode, minimized, afterGeneration) {
    const until = Date.now() + 12000;
    for (;;) {
      const state = JSON.parse(client.state_json());
      if (state.stream_state === 'Live' && state.hosted?.phase === phase &&
          state.hosted?.mode === mode && state.hosted?.minimized === minimized &&
          state.hosted.routing_generation >= afterGeneration) return state.hosted;
      if (Date.now() >= until) throw new Error('FAIL: live host observation missing');
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  }
  function currentReference() {
    const raw = client.state_json();
    const observedEpoch = raw.match(/"epoch"\s*:\s*([0-9]+)/)?.[1];
    const hostedRaw = raw.match(/"hosted"\s*:\s*(\{[^}]+\})/)?.[1];
    const revision = hostedRaw?.match(/"revision"\s*:\s*([0-9]+)/)?.[1];
    if (observedEpoch !== epoch || !revision) throw new Error('FAIL: stale host reference');
    return `{"epoch":${epoch},"revision":${revision}}`;
  }
  function receiptReference(response) {
    const stateRaw = response.__raw?.match(/"state"\s*:\s*(\{[^}]+\})/)?.[1];
    const revision = stateRaw?.match(/"revision"\s*:\s*([0-9]+)/)?.[1];
    if (!revision) throw new Error('FAIL: exact hosted receipt revision missing');
    return `{"epoch":${epoch},"revision":${revision}}`;
  }
  try {
    begin('query');
    client.query_hosted(epoch);
    const queried = await reply('hosted_state', 8000);
    if (queried.state?.phase !== 'native' || queried.state?.mode !== 'native') {
      throw new Error('FAIL: hosted query was not native');
    }
    const firstReference = currentReference();
    begin('switched');
    client.set_hosted_mode(firstReference, '"switched"');
    const switched = await reply('hosted_action');
    if (switched.action?.kind !== 'set_mode' || switched.action?.value !== 'switched' ||
        switched.state?.phase !== 'ready' || switched.state?.mode !== 'switched' ||
        switched.state.routing_generation <= ready.hosted.routing_generation) {
      throw new Error('FAIL: switched worker completion');
    }
    begin('refresh');
    client.refresh_subscription();
    await reply('subscribed', 8000);
    await live('ready', 'switched', false, switched.state.routing_generation);
    begin('stale_revision');
    let staleRejected = false;
    try { client.set_hosted_mode(firstReference, '"concurrent"'); }
    catch (_) { staleRejected = true; }
    if (!staleRejected) throw new Error('FAIL: stale hosted revision was accepted');
    begin('concurrent');
    client.set_hosted_mode(currentReference(), '"concurrent"');
    const concurrent = await reply('hosted_action');
    if (concurrent.action?.value !== 'concurrent' || concurrent.state?.mode !== 'concurrent' ||
        concurrent.state?.phase !== 'ready' ||
        concurrent.state.routing_generation !== switched.state.routing_generation) {
      throw new Error('FAIL: concurrent worker completion');
    }
    // Use the validated action receipt immediately; its stream event may still
    // be queued. The next call must accept that fresh revision.
    begin('minimize');
    client.minimize_hosted(receiptReference(concurrent));
    const minimized = await reply('hosted_action');
    if (minimized.action?.kind !== 'viewport' || minimized.action?.value !== 'minimize' ||
        minimized.state?.minimized !== true) throw new Error('FAIL: minimize observation');
    const caughtUp = await live('ready', 'concurrent', true, concurrent.state.routing_generation);
    if (caughtUp.revision < minimized.state.revision) {
      throw new Error('FAIL: ordered hosted stream did not reconcile');
    }
    window.pimMatchedTimerStarted = Date.now();
    begin('restore');
    client.restore_hosted(currentReference());
    const restored = await reply('hosted_action');
    if (restored.action?.value !== 'restore' || restored.state?.minimized !== false) {
      throw new Error('FAIL: restore observation');
    }
    await live('ready', 'concurrent', false, concurrent.state.routing_generation);
    begin('complete');
    return {status: 'PASS', query: true, switched: true, concurrent: true,
            minimize: true, restore: true, stale_revision: true, refresh_after_receipt: true};
  } finally {
    try { observeState(JSON.parse(client.state_json())); }
    catch (_) { diagnostic.state_read_failed = true; }
    client.set_event_handler(null);
  }
}

// This filter is installed only by the disposable qualification page. It
// preserves the Rust listener's original callback identity for removal and
// passes every frame except the one explicitly armed hosted action receipt.
export function installHostedReplyFilter() {
  const filter = {armed: false, monitoring: false, dropped: false, gapInjected: false, refreshed: false,
                  sent: 0, expectedRequest: null, oldStreamSuppressed: 0,
                  subscription: null, oldSubscription: null};
  const additions = EventTarget.prototype.addEventListener;
  const removals = EventTarget.prototype.removeEventListener;
  const sends = WebSocket.prototype.send;
  const callbacks = new WeakMap();
  EventTarget.prototype.addEventListener = function(type, callback, options) {
    if (!(this instanceof WebSocket) || type !== 'message') {
      return additions.call(this, type, callback, options);
    }
    let mapped = callbacks.get(this);
    if (!mapped) { mapped = new Map(); callbacks.set(this, mapped); }
    const wrapped = (event) => {
      const raw = typeof event.data === 'string' ? event.data : '';
      if (raw.length <= 65536) {
        const frame = JSON.parse(raw);
        if (frame.kind === 'response' && frame.reply?.status === 'ok' &&
            frame.reply.response?.kind === 'subscribed') {
          const subscription = raw.match(/"subscription"\s*:\s*([0-9]+)/)?.[1];
          if (subscription) {
            filter.subscription = subscription;
          }
        }
        if (filter.oldSubscription && frame.kind === 'event_stream' &&
            raw.match(/"subscription"\s*:\s*([0-9]+)/)?.[1] === filter.oldSubscription) {
          filter.oldStreamSuppressed++;
          return;
        }
        if (filter.armed && frame.kind === 'response' && frame.reply?.status === 'ok' &&
            frame.reply.response?.kind === 'hosted_action' &&
            frame.reply.response.action?.kind === 'set_mode' &&
            frame.reply.response.action?.value === 'switched' &&
            raw.match(/"request_id"\s*:\s*([0-9]+)/)?.[1] === filter.expectedRequest &&
            filter.subscription) {
          filter.armed = false;
          filter.dropped = true;
          return;
        }
      }
      callback.call(this, event);
    };
    mapped.set(callback, wrapped);
    filter.injectGap = () => {
      if (!filter.armed || filter.gapInjected || !filter.subscription) {
        throw new Error('FAIL: gap injection was not uniquely armed');
      }
      const sequence = window.pimClient.state_json().match(/"sequence"\s*:\s*([0-9]+)/)?.[1];
      if (!sequence) throw new Error('FAIL: no live sequence for injected gap');
      const lost = (BigInt(sequence) + 1n).toString();
      const gap = `{"kind":"event_stream","subscription":${filter.subscription},` +
        `"stream":{"kind":"gap","after_sequence":${sequence},"lost_through":${lost}}}`;
      filter.oldSubscription = filter.subscription;
      callback.call(this, new MessageEvent('message', {data: gap}));
      filter.gapInjected = true;
    };
    return additions.call(this, type, wrapped, options);
  };
  EventTarget.prototype.removeEventListener = function(type, callback, options) {
    const wrapped = this instanceof WebSocket && type === 'message' ?
      callbacks.get(this)?.get(callback) : null;
    if (wrapped) {
      callbacks.get(this).delete(callback);
      return removals.call(this, type, wrapped, options);
    }
    return removals.call(this, type, callback, options);
  };
  WebSocket.prototype.send = function(data) {
    if (filter.monitoring && typeof data === 'string' &&
        data.includes('"kind":"hosted_set_mode"') && data.includes('"mode":"switched"')) {
      filter.sent++;
      if (filter.sent === 1) {
        filter.expectedRequest = data.match(/"request_id"\s*:\s*([0-9]+)/)?.[1] || null;
      }
    }
    return sends.call(this, data);
  };
  window.pimHostFilter = filter;
}

// The GUI has already completed positive Web calls. A separately approved MCP
// observer confirms the real mode effect while this browser waits for its lost
// receipt. The gap below is fixture-injected, not a claim of server overflow.
export async function runHostedLostReceipt(client) {
  const filter = window.pimHostFilter;
  if (!filter || filter.armed || filter.dropped) throw new Error('FAIL: filter not fresh');
  const raw = client.state_json();
  const epoch = raw.match(/"epoch"\s*:\s*([0-9]+)/)?.[1];
  const hostedRaw = raw.match(/"hosted"\s*:\s*(\{[^}]+\})/)?.[1];
  const revision = hostedRaw?.match(/"revision"\s*:\s*([0-9]+)/)?.[1];
  if (!epoch || !revision || JSON.parse(raw).hosted?.mode !== 'concurrent') {
    throw new Error('FAIL: fault precondition');
  }
  const matchedStarted = window.pimMatchedTimerStarted;
  if (!Number.isInteger(matchedStarted) || Date.now() - matchedStarted < 8000 ||
      Date.now() - matchedStarted >= 30000) {
    throw new Error('FAIL: stale timer overlap was not armed');
  }
  let terminalCallbacks = 0, deadlineCallbacks = 0;
  client.set_event_handler(raw => {
    const frame = JSON.parse(raw);
    if (frame.kind === 'local_disconnected') {
      terminalCallbacks++;
      if (frame.message === 'Hosted request deadline reached; action outcome unconfirmed; reconnect and inspect without replay') {
        deadlineCallbacks++;
      }
    }
  });
  try {
    filter.armed = true;
    filter.monitoring = true;
    const started = Date.now();
    client.set_hosted_mode(`{"epoch":${epoch},"revision":${revision}}`, '"switched"');
    filter.injectGap();
    if (JSON.parse(client.state_json()).stream_state !== 'NeedsSnapshot') {
      throw new Error('FAIL: fixture gap did not fence the tracked view');
    }
    const untilDropped = Date.now() + 15000;
    while (!filter.dropped) {
      if (Date.now() >= untilDropped) throw new Error('FAIL: committed reply not seen by filter');
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    if (!filter.gapInjected) throw new Error('FAIL: fixture gap missing');
    client.refresh_subscription();
    const untilFresh = Date.now() + 10000;
    while (JSON.parse(client.state_json()).stream_state !== 'Live') {
      if (Date.now() >= untilFresh) throw new Error('FAIL: explicit gap refresh did not complete');
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    filter.refreshed = true;
    // The previous successful Restore timer belongs to the same WebClient but
    // its old lease. It must not fence this new lease's still pending action.
    const oldDeadline = matchedStarted + 60000;
    while (Date.now() < oldDeadline + 1500) {
      if (JSON.parse(client.state_json()).stream_state !== 'Live') {
        throw new Error('FAIL: old timer fenced the reconnected client');
      }
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    if (JSON.parse(client.state_json()).stream_state !== 'Live' ||
        Date.now() - started >= 58000) {
      throw new Error('FAIL: newer request did not survive the old timer');
    }
    const untilTimeout = started + 63000;
    while (Date.now() < untilTimeout) {
      const state = JSON.parse(client.state_json());
      if (state.stream_state !== 'Live') break;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    const state = JSON.parse(client.state_json());
    const notice = document.querySelector('[data-status]')?.textContent || '';
    if (state.stream_state === 'Live' || !notice.includes('outcome unconfirmed') ||
        !filter.dropped || !filter.gapInjected || !filter.refreshed || filter.sent !== 1 ||
        !filter.expectedRequest ||
        Date.now() - started < 58000 || terminalCallbacks !== 1 || deadlineCallbacks !== 1) {
      throw new Error('FAIL: lost receipt was not fenced by the browser deadline');
    }
    filter.monitoring = false;
    return {status: 'PASS', dropped_reply: true, fixture_injected_gap: true,
            explicit_refresh: true, browser_deadline: true, no_replay: true,
            stale_timer_reconnect: true, terminal_callback_once: true};
  } finally {
    client.set_event_handler(null);
  }
}
