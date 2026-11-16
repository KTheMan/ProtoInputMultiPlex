// Invoke on the existing disposable example page after its GUI-approved Web
// receiver is connected. The native fixture must be running concurrently.
// This is an actual browser-call gate, not a mocked window state transition.
export async function runWindowRouteAcceptance(client, referenceJson, controlJson = '{"kind":"restore"}', diagnostic = null) {
  // This optional owned-test object receives fixed enums only, never client data.
  const stage = (value) => {
    if (diagnostic) Object.assign(diagnostic, { stage: value, predicate: 'none', predicate_failed: false });
  };
  const fail = (predicate, message) => {
    if (diagnostic) Object.assign(diagnostic, { predicate, predicate_failed: true });
    return new Error(message);
  };
  stage('reference_state');
  // Window references contain exact u64 values. Keep the GUI's JSON text intact
  // when calling the Rust methods; JavaScript Number can round the epoch or ID.
  const reference = JSON.parse(referenceJson);
  const stateJson = client.state_json();
  const before = JSON.parse(stateJson);
  const epochDigits = (json) => json.match(/"epoch"\s*:\s*([0-9]+)/)?.[1];
  if (!epochDigits(referenceJson) || epochDigits(referenceJson) !== epochDigits(stateJson)) {
    throw fail('epoch_current', 'FAIL: stale window epoch');
  }
  if (before.control_grant?.window_publication || !before.event_grant?.owned_windows ||
      !before.control_grant?.window_tracking || !before.control_grant?.window_control) {
    throw fail('window_authority', 'FAIL: browser has the wrong window authority');
  }
  const queued = [];
  let wake;
  stage('event_handler');
  client.set_event_handler((json) => {
    const frame = JSON.parse(json);
    queued.push(frame);
    if (wake) { wake(); wake = undefined; }
  });
  async function response(kind) {
    const deadline = Date.now() + 6000;
    for (;;) {
      const index = queued.findIndex((frame) => frame.kind === 'response' &&
        frame.reply?.status === 'ok' && frame.reply.response?.kind === kind);
      if (index >= 0) return queued.splice(index, 1)[0].reply.response;
      if (queued.some((frame) => frame.kind === 'response' && frame.reply?.status === 'error')) {
        throw fail('request_accepted', 'FAIL: window request was rejected');
      }
      if (Date.now() >= deadline) throw fail('response_before_deadline', `FAIL: ${kind} receipt timed out; outcome uncertain`);
      await new Promise((resolve) => { wake = resolve; setTimeout(resolve, 50); });
    }
  }
  async function trackedState(windowId, present) {
    const deadline = Date.now() + 6000;
    for (;;) {
      const state = JSON.parse(client.state_json());
      if (state.stream_state !== 'Live') throw fail('stream_live', 'FAIL: window stream is not live');
      if (state.windows?.some((window) => window.id === windowId) === present) return;
      if (Date.now() >= deadline) throw fail('tracked_state_before_deadline', 'FAIL: tracked window event timed out');
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  }
  try {
    stage('query_request');
    client.query_window(referenceJson);
    stage('query_response');
    const queried = await response('window');
    stage('query_validate');
    if (queried.window.id !== reference.window ||
        queried.window.metadata?.title !== 'Owned SDK fixture window' ||
        queried.window.metadata?.process_id != null ||
        queried.window.metadata?.ui_thread_id != null ||
        queried.window.bounds?.left !== 150 || queried.window.bounds?.top !== 120 ||
        queried.window.bounds?.right !== 550 || queried.window.bounds?.bottom !== 420 ||
        queried.window.observation?.source !== 'native_client_reported') {
      throw fail('query_native_provenance', 'FAIL: query lacked native-client provenance');
    }
    stage('track_request');
    client.track_window(referenceJson);
    stage('track_response');
    const trackedReply = await response('window');
    stage('track_validate');
    if (trackedReply.window.id !== reference.window) {
      throw fail('tracked_window_matches', 'FAIL: tracking returned another window');
    }
    stage('track_event');
    await trackedState(queried.window.id, true);
    stage('control_request');
    client.control_window(referenceJson, controlJson);
    stage('control_response');
    const applied = await response('window_controlled');
    stage('control_validate');
    if (applied.acknowledgement?.window !== reference.window ||
        applied.acknowledgement?.observed?.id !== reference.window ||
        applied.acknowledgement?.observed?.observation?.stale ||
        applied.acknowledgement?.observed?.observation?.source !== 'native_client_reported') {
      throw fail('owner_receipt_fresh', 'FAIL: owner action lacked a fresh observed receipt');
    }
    stage('untrack_request');
    client.untrack_window(referenceJson);
    stage('untrack_response');
    await response('acknowledged');
    stage('untrack_event');
    await trackedState(queried.window.id, false);
    stage('complete');
    return { status: 'PASS', query: true, track: true, owner_action: true, untrack: true };
  } finally {
    client.set_event_handler(null);
  }
}
