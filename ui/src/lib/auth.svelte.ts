// The signed-in state of the UI: `/$/auth/config` and `/$/whoami`, and the request
// hooks (CSRF header, sign-in on 401).

import { goto } from '$app/navigation';
import { page } from '$app/state';
import { setAuthHooks } from './api';
import * as authApi from './auth-api';
import {
  can,
  hasServer,
  loginHref,
  signedIn,
  type AuthConfig,
  type Level,
  type ServerPerm,
  type Whoami,
} from './auth';

/** Pages that must stay reachable without signing in. */
function publicPath(path: string) {
  return path === '/ui/login' || path.startsWith('/ui/login/');
}

class AuthStore {
  config = $state<AuthConfig | null>(null);
  who = $state<Whoami | null>(null);
  loaded = $state(false);
  error = $state<string | null>(null);

  constructor() {
    setAuthHooks({
      csrf: () => this.who?.csrfToken,
      unauthorized: () => void this.unauthorized(),
      ready: () => this.ensure(),
    });
  }

  get enabled() {
    return this.who?.authEnabled === true;
  }

  get signedIn() {
    return signedIn(this.who);
  }

  can(ds: string | null | undefined, need: Level) {
    return can(this.who, ds, need);
  }

  hasServer(perm: ServerPerm) {
    return hasServer(this.who, perm);
  }

  #first: Promise<void> | null = null;

  /** The first load (started now if needed); later calls reuse it. */
  ensure(): Promise<void> {
    this.#first ??= this.load();
    return this.#first;
  }

  async load() {
    try {
      const [config, who] = await Promise.all([authApi.authConfig(), authApi.whoami()]);
      this.config = config;
      this.who = who;
      this.error = null;
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
    } finally {
      this.loaded = true;
    }
  }

  #recheck: Promise<void> | null = null;

  /**
   * A request was refused with 401. The caller is loaded again before anything else,
   * since the session may have ended (it expired, or the user signed out elsewhere), and
   * the sign-in page decides from that state. Without the reload it would see the caller
   * still signed in and send the user straight back to a page that gets 401 again, in a
   * loop. When the server still knows the caller as signed in, the 401 was about
   * something else and the page stays where it is.
   */
  async unauthorized() {
    if (!this.enabled) return;
    const wasSignedIn = signedIn(this.who);
    this.#recheck ??= this.load().finally(() => (this.#recheck = null));
    await this.#recheck;
    if (wasSignedIn && signedIn(this.who)) return;
    this.toLogin();
  }

  /** Go to the sign-in page (and come back here afterwards). */
  toLogin() {
    const path = page.url.pathname;
    if (!this.enabled || publicPath(path)) return;
    void goto(loginHref(path + page.url.search), { replaceState: true });
  }

  /** After loading: an anonymous caller who can see nothing is sent to sign in. */
  guard() {
    const who = this.who;
    if (!who?.authEnabled || who.principal.kind !== 'anonymous') return;
    if (Object.keys(who.datasets).length > 0) return;
    this.toLogin();
  }

  async logout() {
    const r = await authApi.logout();
    if (r.redirect) {
      window.location.href = r.redirect;
      return;
    }
    await this.load();
    await goto(loginHref('/ui/'));
  }
}

export const auth = new AuthStore();
