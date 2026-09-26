// Prefer a page-owned WebSocket. Privileged HTTP remains available for sites
// whose CSP or mixed-content rules prohibit connecting to the configured URL.
import { createConnectionLog } from '../lib/connectionLog.js';

export function createPushConnection({ url, fallback, apply, status, socketFactory = url => new WebSocket(url), schedule = setTimeout, cancel = clearTimeout }) {
  let socket, retry, watchdog, stopped = true, usingFallback = false;
  let activeLog;

  function startFallback(reason) {
    if (!usingFallback && !stopped) {
      usingFallback = true;
      activeLog?.event('http_fallback_started', { transport: 'http_fallback', reason }, 'warn');
      fallback.start();
    }
  }

  function open() {
    if (stopped) return;
    let current, diagnostics;
    const failed = reason => {
      if (stopped || socket !== current) return;
      diagnostics.event('websocket_unavailable', { reason }, 'warn');
      cancel(watchdog);
      socket = null;
      if (current) {
        current.onmessage = current.onerror = null;
        // Keep the close listener long enough to record the actual close code,
        // even when the browser emits its uninformative error event first.
        current.close();
      }
      startFallback(reason);
      diagnostics.event('reconnect_scheduled', { retry_in_ms: 10_000, reason });
      retry = schedule(open, 10_000);
    };
    let address = '';
    try {
      address = url();
      diagnostics = createConnectionLog('companion', address);
      activeLog = diagnostics;
      current = socketFactory(address);
      socket = current;
    } catch (error) {
      diagnostics ??= createConnectionLog('companion', address);
      activeLog = diagnostics;
      diagnostics.error(error);
      startFallback('websocket_constructor_failed');
      diagnostics.event('reconnect_scheduled', { retry_in_ms: 30_000, reason: 'websocket_constructor_failed' });
      retry = schedule(open, 30_000);
      return;
    }
    // Keep a blocked upgrade from delaying initial loading.
    watchdog = schedule(() => startFallback('no_initial_message_within_1000ms'), 1000);
    current.onopen = () => {
      if (!stopped && socket === current) diagnostics.opened();
    };
    current.onerror = error => {
      diagnostics.error(error, current.readyState);
      failed('socket_error');
    };
    current.onclose = event => {
      diagnostics.closed(event);
      failed('socket_closed');
    };
    current.onmessage = event => {
      if (stopped || socket !== current) return;
      try {
        const message = JSON.parse(event.data);
        if (!['init', 'full_update', 'position', 'new_card', 'enhancement_result', 'remote_result', 'review_saved'].includes(message.type)) return;
        diagnostics.message(message);
        if (usingFallback || message.type === 'init') {
          if (usingFallback) diagnostics.event('http_fallback_stopped', { reason: 'websocket_messages_arriving' });
          usingFallback = false;
          fallback.stop();
        }
        cancel(watchdog);
        watchdog = schedule(() => failed('no_messages_within_5000ms'), 5000);
        apply(message);
        status(null);
      } catch (error) {
        diagnostics.event('message_processing_failed', { message: error.message }, 'warn');
      }
    };
  }

  function stop() {
    if (!stopped) activeLog?.event('connection_stopped', { reason: 'client_suspended_or_restarted', transport: usingFallback ? 'http_fallback' : 'websocket' });
    stopped = true;
    cancel(retry); cancel(watchdog);
    if (socket) {
      socket.onopen = socket.onmessage = socket.onerror = socket.onclose = null;
      socket.close(); socket = null;
    }
    fallback.stop();
    usingFallback = false;
  }
  return { start() { stop(); stopped = false; open(); }, stop };
}
