// One poller of `GET /$/tasks` for the whole UI. Every task list and every page that
// waits for tasks to finish subscribes here, so the endpoint has a single timer however
// many of them are mounted. It polls fast while a task runs and backs off while none does.

import * as api from './api';
import { poll, type Poller } from './poll';

/** While a task is queued or running. */
const BUSY_MS = 1000;
/** With no task running: the first delay, doubling up to the second. */
const IDLE_MS = 5000;
const MAX_IDLE_MS = 60_000;

export const taskActive = (t: api.Task) => t.state === 'running' || t.state === 'queued';

type Listener = (tasks: api.Task[]) => void;

class TaskFeed {
  tasks = $state<api.Task[]>([]);
  error = $state<string | null>(null);
  loaded = $state(false);
  #listeners = new Set<Listener>();
  #poller: Poller | null = null;

  /**
   * Calls `fn` with every new list (and with the current one, if loaded) while
   * subscribed. The first subscriber starts the poller and the last one stops it.
   */
  subscribe(fn: Listener): () => void {
    this.#listeners.add(fn);
    if (!this.#poller)
      this.#poller = poll(() => this.#load(), {
        interval: BUSY_MS,
        idle: IDLE_MS,
        maxIdle: MAX_IDLE_MS,
      });
    else if (this.loaded) fn(this.tasks);
    return () => {
      this.#listeners.delete(fn);
      if (this.#listeners.size === 0) {
        this.#poller?.stop();
        this.#poller = null;
      }
    };
  }

  /** Load now (a task was just started) and poll fast again. */
  refresh(): Promise<void> {
    return this.#poller?.refresh() ?? Promise.resolve();
  }

  async #load(): Promise<boolean> {
    try {
      const list = await api.listTasks();
      list.sort((a, b) => (b.startedAt ?? '').localeCompare(a.startedAt ?? ''));
      this.tasks = list;
      this.error = null;
    } catch (e) {
      this.error = api.errorMessage(e);
    } finally {
      this.loaded = true;
    }
    for (const fn of this.#listeners) fn(this.tasks);
    return this.tasks.some(taskActive);
  }
}

export const taskFeed = new TaskFeed();

/**
 * Calls `ondone` for each task that finishes while subscribed: one seen running, or one
 * that started and finished between two polls and passes `mine`.
 */
export function followTasks(
  ondone: (t: api.Task) => void,
  mine: (t: api.Task) => boolean = () => true,
): () => void {
  const running = new Set<string>();
  const seen = new Set<string>();
  let first = true;
  return taskFeed.subscribe((list) => {
    for (const t of list) {
      if (taskActive(t)) running.add(t.id);
      else if (running.has(t.id) || (!first && !seen.has(t.id) && mine(t))) {
        running.delete(t.id);
        ondone(t);
      }
      seen.add(t.id);
    }
    first = false;
  });
}
