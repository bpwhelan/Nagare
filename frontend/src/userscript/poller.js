// Transport-independent polling loop, also exercised with delayed mock replies.
// One request at a time; stop/restart invalidates every outstanding response.
export function createPoller({ request, apply, status, requestState, interval, longPoll = false, schedule = setTimeout, cancel = clearTimeout }) {
  let generation = 0;
  let timer;
  let stopped = true;
  let epoch;
  let cursor;
  let subtitleRevision;

  async function poll(run) {
    const before = requestState();
    let delay = interval();
    try {
      if (before.mutations) return;
      const query = new URLSearchParams();
      if (epoch) query.set('epoch', epoch);
      if (cursor != null) query.set('after', cursor);
      if (subtitleRevision) query.set('subtitle_revision', subtitleRevision);
      if (longPoll) query.set('wait_ms', String(delay));
      const result = await request(`/api/companion?${query}`);
      if (run !== generation || stopped) return;
      const after = requestState();
      if (before.revision !== after.revision || after.mutations) return;
      if (result.protocol !== 1 || !result.snapshot || !result.epoch) {
        throw new Error('This Nagare server does not support the Companion protocol. Update Nagare and reinstall the userscript.');
      }
      apply(result, epoch != null && epoch !== result.epoch);
      epoch = result.epoch;
      cursor = result.cursor;
      subtitleRevision = result.subtitle_revision;
      status(null);
      // The server has already waited, and wakes on a new card/result. Start
      // listening again immediately instead of adding a client polling delay.
      if (longPoll) delay = 0;
    } catch (error) {
      if (run !== generation || stopped) return;
      status(error.message || 'Connection lost. Retrying…');
      delay = Math.max(delay, 3000);
    } finally {
      if (!stopped && run === generation) timer = schedule(() => poll(run), delay);
    }
  }

  function stop() {
    stopped = true;
    generation++;
    cancel(timer);
  }

  return {
    start() {
      stop();
      stopped = false;
      epoch = cursor = subtitleRevision = undefined;
      poll(generation);
    },
    stop,
  };
}
