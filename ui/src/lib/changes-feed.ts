// The change feed as server-sent events (docs/API.md, "Change feed"): the time slider's
// Follow switch listens to it for new commits while the slider rests on the head.
import { ApiError, request } from './api';
import { SseParser } from './ask-api';

/** The part of a `commit` event the slider needs. */
export type FeedCommit = { seq: number; timestamp?: string; kind?: string; message?: string };

/**
 * Open `GET /{ds}/changes` as an event stream after commit `after` and call `on` for each
 * commit made since, including the ones made before the stream opened. Resolves with the
 * number of the last commit delivered (`after` when there was none) when the stream ends,
 * which the server does after five minutes, so that the next call resumes from it. Rejects
 * with the `ApiError` when it cannot be opened. Abort `signal` to stop.
 */
export async function followCommits(
  ds: string,
  after: number,
  on: (c: FeedCommit) => void,
  signal: AbortSignal,
): Promise<number> {
  let last = after;
  const res = await request(`/${encodeURIComponent(ds)}/changes?limit=1&after=commit:${after}`, {
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
        if (typeof c.seq === 'number') {
          last = Math.max(last, c.seq);
          on(c);
        }
      } catch {
        /* not JSON: skipped */
      }
    }
  };
  const reader = res.body?.getReader();
  if (!reader) return last;
  const dec = new TextDecoder();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    deliver(dec.decode(value, { stream: true }));
  }
  return last;
}

/** Why Follow stopped, in a sentence. */
export function changesFeedUnavailable(e: unknown): string {
  if (e instanceof ApiError && (e.status === 403 || e.status === 404))
    return 'This server does not offer the change feed to you.';
  return `Follow stopped. ${e instanceof Error ? e.message : String(e)}`;
}
