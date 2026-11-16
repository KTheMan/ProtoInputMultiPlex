// Test-only driver for the actual Rust/WASM client. Preserve opaque u64s as JSON text.
export function installDesktopAcceptance(client) {
  const state = {messages: [], clicks: [], errors: 0};
  client.set_event_handler(raw => {
    if (typeof raw !== "string" || state.messages.length >= 4096) throw Error("desktop callback bound");
    state.messages.push(raw);
  });
  return {
    messages: () => JSON.stringify(state.messages),
    clicks: () => JSON.stringify(state.clicks),
    clear: () => { state.messages.length = 0; state.clicks.length = 0; },
    installClick: (screenJson, pointJson) => {
      // These coordinates are bounded i32s; identities remain unparsed strings.
      const screen = JSON.parse(screenJson), point = JSON.parse(pointJson);
      const root = document.getElementById("application-demo");
      const canvas = root.querySelector("[data-canvas]");
      const rect = canvas.getBoundingClientRect();
      const hit = document.createElement("button");
      hit.id = "provider-hit"; hit.textContent = "Virtual observer hit target";
      Object.assign(hit.style, {position: "fixed", width: "14px", height: "14px", padding: "0", border: "0",
        left: `${rect.left + (point.x-screen.left)/(screen.right-screen.left)*rect.width - 7}px`,
        top: `${rect.top + (point.y-screen.top)/(screen.bottom-screen.top)*rect.height - 7}px`});
      root.appendChild(hit);
      client.bind_virtual_click('{"selector":"#provider-hit","button":"x1"}', raw => {
        if (typeof raw !== "string" || state.clicks.length >= 16) throw Error("desktop click bound");
        state.clicks.push(raw);
      });
      return true;
    },
    deny: commandStringsJson => {
      const commands = JSON.parse(commandStringsJson);
      if (!Array.isArray(commands) || commands.length !== 4 || commands.some(x => typeof x !== "string")) throw Error("desktop denial fixture schema");
      let denied = 0;
      for (const raw of commands) { try { client.request(raw); } catch (_) { denied++; } }
      return denied;
    }
  };
}
