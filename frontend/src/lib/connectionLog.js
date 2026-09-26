let sequence = 0;

function endpointDetails(address) {
  try {
    const endpoint = new URL(address);
    return {
      // Keep the proxy hostname/path visible, but omit credentials and tokens.
      url: `${endpoint.protocol}//${endpoint.host}${endpoint.pathname}`,
      secure: endpoint.protocol === 'wss:' || endpoint.protocol === 'https:',
    };
  } catch { return { url: '(invalid URL)', secure: false }; }
}

export function logConnection(client, stage, address, details = {}, level = 'info') {
  const page = globalThis.location;
  console[level](`[Nagare connection] ${client}: ${stage}`, {
    timestamp: new Date().toISOString(),
    client,
    stage,
    ...endpointDetails(address),
    page_origin: page?.origin || (page?.host ? `${page.protocol}//${page.host}` : null),
    visibility: globalThis.document?.visibilityState ?? 'unknown',
    online: globalThis.navigator?.onLine ?? null,
    ...details,
  });
}

export function logTransportEvent(client, transport, address, message, details = {}) {
  if (!['new_card', 'enhancement_result'].includes(message.type)) return;
  logConnection(client, 'event_received', address, {
    ...details,
    transport,
    message_type: message.type,
    note_id: message.new_card?.event?.note_id ?? message.enhancement_result?.note_id,
    ...(message.enhancement_result ? { success: message.enhancement_result.success } : {}),
  });
}

// One record per connection attempt. No per-position logging: the feed normally
// sends 20 messages per second. Timings use this browser's monotonic clock.
export function createConnectionLog(client, address) {
  const connectionId = `${client}-${++sequence}`;
  const started = performance.now();
  let opened = null, lastMessage = null, messages = 0;
  const stats = () => ({
    connection_id: connectionId,
    transport: 'websocket',
    connection_age_ms: Math.round(performance.now() - started),
    received_messages: messages,
    last_message_age_ms: lastMessage == null ? null : Math.round(performance.now() - lastMessage),
  });
  const event = (stage, details = {}, level = 'info') => logConnection(client, stage, address, { ...stats(), ...details }, level);
  event('websocket_connecting', {
    mixed_content_candidate: globalThis.location?.protocol === 'https:' && address.startsWith('ws:'),
  });
  return {
    event,
    opened() {
      opened = performance.now();
      event('websocket_open', { handshake_ms: Math.round(opened - started), message: 'WebSocket upgrade succeeded.' });
    },
    message(message) {
      const now = performance.now();
      const gap = lastMessage == null ? null : Math.round(now - lastMessage);
      lastMessage = now;
      messages++;
      if (messages === 1) {
        event('websocket_ready', {
          first_message_type: message.type,
          first_message_ms: Math.round(now - started),
          after_open_ms: opened == null ? null : Math.round(now - opened),
          pending_cards: message.pending_cards?.length,
          message: 'Server messages are arriving over WebSocket.',
        });
      } else if (gap >= 2000) {
        event('websocket_traffic_gap', { gap_ms: gap }, 'warn');
      }
      logTransportEvent(client, 'websocket', address, message, { ...stats(), preceding_message_gap_ms: gap });
    },
    closed(close = {}) {
      event('websocket_closed', {
        close_code: close.code ?? null,
        close_reason: close.reason || '(not supplied)',
        was_clean: close.wasClean ?? null,
      }, close.wasClean ? 'info' : 'warn');
    },
    error(error, readyState) {
      event('websocket_error', {
        ready_state: readyState ?? null,
        error_name: error?.name || error?.type || 'Error',
        message: error?.message || 'The browser did not provide a cause. Inspect the /ws request in DevTools Network for the handshake status.',
      }, 'warn');
    },
  };
}
