// The change feed as server-sent events (docs/API.md, "Change feed"): the time slider's
// Follow switch listens to it for new commits while the slider rests on the head.
import { ApiError, request } from './api';
import { SseParser } from './ask-api';

/** The part of a `commit` event the slider needs. */
export type FeedCommit = { seq: number; timestamp?: string; kind?: string; message?: string };

/**
 * Open `GET /{ds}/changes` as an event stream after the head and call `on` for each commit
 * made from now on. Resolves when the stream ends (the server closes it after five minutes)
 * and rejects with the `ApiError` when it cannot be opened. Abort `signal` to stop.
 */
export async function followCommits(
  ds: string,
  on: (c: FeedCommit) => void,
  signal: AbortSignal,
): Promise<void> {
  const res = await request(`/${encodeURIComponent(ds)}/changes?limit=1`, {
    headers: { Accept: 'text/event-stream' },
    signal,
  });
  const parser = new SseParser();
  const deliver = (text: string) => {
    for (const { event, data } of parser.push(text)) {
      if (event !== 'commit') continue;
      try {
        const body = JSON.parse(data) as { commit?: FeedCommit } & FeedCommit;
        const c = body.commit ?? body;
        if (typeof c.seq === 'number') on(c);
      } catch {
        /* not JSON: skipped */
      }
    }
  };
  const reader = res.body?.getReader();
  if (!reader) return;
  const dec = new TextDecoder();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    deliver(dec.decode(value, { stream: true }));
  }
}

/** Why Follow stopped, in a sentence. */
export function changesFeedUnavailable(e: unknown): string {
  if (e instanceof ApiError && (e.status === 403 || e.status === 404))
    return 'This server does not offer the change feed to you.';
  return `Follow stopped. ${e instanceof Error ? e.message : String(e)}`;
}
