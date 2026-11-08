// Invoke on the existing disposable example page after its GUI-approved Web
// receiver is connected. The native fixture must be running concurrently.
// This is an actual browser-call gate, not a mocked window state transition.
export async function runWindowRouteAcceptance(client, referenceJson, controlJson = '{"kind":"restore"}') {
  // Window references contain exact u64 values. Keep the GUI's JSON text intact
  // when calling the Rust methods; JavaScript Number can round the epoch or ID.
  const reference = JSON.parse(referenceJson);
  const stateJson = client.state_json();
  const before = JSON.parse(stateJson);
  const epochDigits = (json) => json.match(/"epoch"\s*:\s*([0-9]+)/)?.[1];
  if (!epochDigits(referenceJson) || epochDigits(referenceJson) !== epochDigits(stateJson)) {
    throw new Error('FAIL: stale window epoch');
  }
  if (before.control_grant?.window_publication || !before.event_grant?.owned_windows ||
      !before.control_grant?.window_tracking || !before.control_grant?.window_control) {
    throw new Error('FAIL: browser has the wrong window authority');
  }
  const queued = [];
  let wake;
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
        throw new Error('FAIL: window request was rejected');
      }
      if (Date.now() >= deadline) throw new Error(`FAIL: ${kind} receipt timed out; outcome uncertain`);
      await new Promise((resolve) => { wake = resolve; setTimeout(resolve, 50); });
    }
  }
  async function trackedState(windowId, present) {
    const deadline = Date.now() + 6000;
    for (;;) {
      const state = JSON.parse(client.state_json());
      if (state.stream_state !== 'Live') throw new Error('FAIL: window stream is not live');
      if (state.windows?.some((window) => window.id === windowId) === present) return;
      if (Date.now() >= deadline) throw new Error('FAIL: tracked window event timed out');
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  }
  try {
    client.query_window(referenceJson);
    const queried = await response('window');
    if (queried.window.id !== reference.window ||
        queried.window.metadata?.title !== 'Owned SDK fixture window' ||
        queried.window.metadata?.process_id != null ||
        queried.window.metadata?.ui_thread_id != null ||
        queried.window.bounds?.left !== 150 || queried.window.bounds?.top !== 120 ||
        queried.window.bounds?.right !== 550 || queried.window.bounds?.bottom !== 420 ||
        queried.window.observation?.source !== 'native_client_reported') {
      throw new Error('FAIL: query lacked native-client provenance');
    }
    client.track_window(referenceJson);
    const trackedReply = await response('window');
    if (trackedReply.window.id !== reference.window) {
      throw new Error('FAIL: tracking returned another window');
    }
    await trackedState(queried.window.id, true);
    client.control_window(referenceJson, controlJson);
    const applied = await response('window_controlled');
    if (applied.acknowledgement?.window !== reference.window ||
        applied.acknowledgement?.observed?.id !== reference.window ||
        applied.acknowledgement?.observed?.observation?.stale ||
        applied.acknowledgement?.observed?.observation?.source !== 'native_client_reported') {
      throw new Error('FAIL: owner action lacked a fresh observed receipt');
    }
    client.untrack_window(referenceJson);
    await response('acknowledged');
    await trackedState(queried.window.id, false);
    return { status: 'PASS', query: true, track: true, owner_action: true, untrack: true };
  } finally {
    client.set_event_handler(null);
  }
}
