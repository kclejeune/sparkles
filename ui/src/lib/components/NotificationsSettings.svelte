<script lang="ts">
  // The server page's Notifications section (C21 §11), for server administrators: the
  // channels with their delivery status, a test send per channel, forms to add a channel
  // or a route, the active conditions and the recent deliveries from `GET
  // /$/notifications`, and the `notifications` kind as a layered settings kind with the
  // same sources, locks and resets as the Models section. A route is the subscription of
  // an event type to channels. Credentials are named by secrets, whose values are set
  // write-only through the secret fields or the Models section's API keys.
  import { onMount } from 'svelte';
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import { fmtTime } from '$lib/format';
  import { poll } from '$lib/poll';
  import { listSecrets, putSecret, secretState, type SecretInfo } from '$lib/models-settings';
  import {
    CHANNEL_TYPES,
    EVENT_TYPES,
    NOTIFICATIONS_URL,
    ROUTES_GROUP,
    channelNames,
    channelTypes,
    emptyChannel,
    groupChannel,
    newChannelPatch,
    newRoutePatch,
    newestFirst,
    notificationFields,
    notificationStatus,
    outcomeText,
    removeChannelPatch,
    removeRoutePatch,
    removedChannels,
    resultClass,
    routeDeclared,
    routeErrors,
    routeKeys,
    routesTo,
    secretLabel,
    testChannel,
    type NotificationStatus,
  } from '$lib/notifications';
  import {
    fieldInfo,
    parsePath,
    patchKind,
    pathString,
    readKind,
    resetKind,
    writeFailure,
    type FieldDef,
    type FormValues,
    type JsonObject,
    type SettingsKind,
  } from '$lib/settings';
  import Icon from './Icon.svelte';
  import Modal from './Modal.svelte';
  import SettingsKindPanel, { type FieldExtras } from './SettingsKindPanel.svelte';
  import type { SecretView } from './SecretField.svelte';

  const STATUS_POLL_MS = 10_000;

  let status = $state<NotificationStatus | null>(null);
  let statusError = $state<string | null>(null);
  let secrets = $state<SecretInfo[] | null>(null);
  /** The `notifications` kind as the panel last showed it. */
  let kind = $state<SettingsKind | null>(null);
  let kindError = $state<string | null>(null);
  let busy = $state(false);

  let addOpen = $state(false);
  let add = $state(emptyChannel());
  let addError = $state<string | null>(null);

  let routeOpen = $state(false);
  let routeKey = $state('');
  let routeChannels = $state<string[]>([]);
  let routeError = $state<string | null>(null);

  let removeChannel = $state<string | null>(null);
  let removeRoute = $state<string | null>(null);
  /** The channels a test is being sent to. */
  let testing = $state<Record<string, boolean>>({});
  /** The answer of the last test of each channel. */
  let tested = $state<Record<string, { ok: boolean; text: string }>>({});

  const fields = $derived(notificationFields(kind));
  const names = $derived(channelNames(kind));
  // the panel starts again when the channels, their types or the routes change, since its
  // form has a group of fields for each channel and a field for each route
  const fieldsKey = $derived(JSON.stringify([channelTypes(kind), routeKeys(kind)]));
  const removed = $derived(removedChannels(kind));
  const routes = $derived((kind?.effective.routes as Record<string, string[]> | undefined) ?? {});
  const recent = $derived(newestFirst(status?.recent ?? []));

  const checkRoutes = (form: FormValues, effective: JsonObject) => routeErrors(effective, form);

  /** A route's list suggests the channels. */
  function extras(d: FieldDef): FieldExtras {
    const p = parsePath(d.path);
    if (p[0] === 'routes') return { suggestions: names };
    return {};
  }

  const secretOf = (name: string) => secrets?.find((s) => s.name === name);

  /** The secrets the channels name, masked, and a way to store a value for one. */
  const secretFields = $derived({
    names: (secrets ?? []).map((s) => s.name),
    state: (name: string): SecretView | null => {
      const s = secretOf(name);
      if (!s) return null;
      const v = secretState(s);
      return { set: v.set, label: v.label, title: v.title, locked: s.locked };
    },
    store: async (name: string, value: string) => {
      try {
        await putSecret(name, value);
        toasts.push('success', `Saved a new value for ${name}`);
        await loadSecrets();
        return true;
      } catch (e) {
        toasts.error(`Could not save the value for ${name}`, e);
        return false;
      }
    },
  });

  async function loadStatus(): Promise<boolean> {
    try {
      status = await notificationStatus();
      statusError = null;
      return status.queued > 0 || status.channels.some((c) => c.inFlight);
    } catch (e) {
      statusError = api.errorMessage(e);
      return false;
    }
  }

  async function loadSecrets() {
    try {
      secrets = (await listSecrets()).secrets;
    } catch {
      // the secret states are a hint; the channels work without them
      secrets = null;
    }
  }

  async function loadKind() {
    try {
      kind = await readKind(NOTIFICATIONS_URL);
      kindError = null;
    } catch (e) {
      kindError = api.errorMessage(e);
    }
  }

  onMount(() => {
    void loadKind();
    void loadSecrets();
    // the queues and the recent deliveries change on their own
    const poller = poll(loadStatus, {
      interval: STATUS_POLL_MS,
      idle: STATUS_POLL_MS,
      maxIdle: 6 * STATUS_POLL_MS,
    });
    return () => poller.stop();
  });

  function refresh() {
    void loadKind();
    void loadStatus();
    void loadSecrets();
  }

  /** After a change of the kind: the status and the secrets follow it. */
  function onKind(k: SettingsKind) {
    const changed = kind?.etag !== k.etag;
    kind = k;
    if (changed) {
      void loadStatus();
      void loadSecrets();
    }
  }

  /**
   * A write outside the panel, with the ETag the panel last read. A 412 reloads the
   * kind, and any other refusal is shown with the server's message.
   */
  async function write(what: string, failed: string, fn: (etag?: string) => Promise<SettingsKind>) {
    busy = true;
    try {
      kind = await fn(kind?.etag);
      toasts.push('success', what);
      void loadStatus();
      void loadSecrets();
      return true;
    } catch (e) {
      if (writeFailure(e).kind === 'stale') {
        await loadKind();
        toasts.push(
          'info',
          'The notification configuration changed elsewhere',
          'It now shows the latest version. Make your change again.',
        );
      } else toasts.error(failed, e);
      return false;
    } finally {
      busy = false;
    }
  }

  async function addChannel(e: SubmitEvent) {
    e.preventDefault();
    const r = newChannelPatch(names, add);
    if ('error' in r) {
      addError = r.error;
      return;
    }
    addError = null;
    const name = add.name.trim();
    if (
      await write(`Added channel ${name}`, `Could not add channel ${name}`, (etag) =>
        patchKind(NOTIFICATIONS_URL, r.patch, etag),
      )
    ) {
      addOpen = false;
      add = emptyChannel();
    }
  }

  async function addRoute(e: SubmitEvent) {
    e.preventDefault();
    const r = newRoutePatch(kind, routeKey, routeChannels);
    if ('error' in r) {
      routeError = r.error;
      return;
    }
    routeError = null;
    const key = routeKey.trim();
    if (
      await write(`Added a route for ${key}`, `Could not add a route for ${key}`, (etag) =>
        patchKind(NOTIFICATIONS_URL, r.patch, etag),
      )
    ) {
      routeOpen = false;
      routeKey = '';
      routeChannels = [];
    }
  }

  async function confirmRemoveChannel() {
    const name = removeChannel;
    if (!name) return;
    const ok = await write(`Removed channel ${name}`, `Could not remove channel ${name}`, (etag) =>
      patchKind(NOTIFICATIONS_URL, removeChannelPatch(kind, name), etag),
    );
    if (ok) {
      removeChannel = null;
      delete tested[name];
    }
  }

  async function confirmRemoveRoute() {
    const key = removeRoute;
    if (key == null) return;
    const ok = await write(
      `Removed the route for ${key}`,
      `Could not remove the route for ${key}`,
      (etag) => patchKind(NOTIFICATIONS_URL, removeRoutePatch(key), etag),
    );
    if (ok) removeRoute = null;
  }

  function restoreChannel(path: string, name: string) {
    void write(
      `Channel ${name} uses the server config again`,
      `Could not restore channel ${name}`,
      (etag) => resetKind(NOTIFICATIONS_URL, path, etag),
    );
  }

  async function sendTest(name: string) {
    testing[name] = true;
    try {
      const r = await testChannel(name);
      tested[name] = { ok: true, text: `Delivered, ${r.status} in ${r.latencyMs} ms` };
      toasts.push('success', `Sent a test to ${name}`);
    } catch (e) {
      tested[name] = { ok: false, text: api.errorMessage(e) };
      toasts.error(`The test to ${name} failed`, e);
    } finally {
      testing[name] = false;
      void loadStatus();
    }
  }

  /** Whether the operator locked the channel or a field of it, which keeps it. */
  const channelLocked = (name: string) => {
    if (!kind) return false;
    const f = fieldInfo(kind, pathString(['channels', name]));
    return f.locked || f.partlyLocked;
  };

  const routeLocked = (key: string) =>
    !!kind && fieldInfo(kind, pathString(['routes', key])).locked;

  /** The last delivery of a channel, success or failure, whichever is newer. */
  function lastOf(c: NotificationStatus['channels'][number]) {
    const s = c.lastSuccess;
    const f = c.lastFailure;
    if (s && (!f || s.at >= f.at)) return { ok: true, d: s };
    if (f) return { ok: false, d: f };
    return null;
  }

  function toggleRouteChannel(name: string, on: boolean) {
    routeChannels = on
      ? [...routeChannels.filter((c) => c !== name), name]
      : routeChannels.filter((c) => c !== name);
  }
</script>

<section class="notify" aria-label="Notifications" data-testid="notifications-section">
  <div class="panel">
    <div class="panel-head">
      <h2>Notifications</h2>
      {#if status}
        <span
          class="badge {status.enabled ? 'ok' : ''}"
          title={status.enabled
            ? 'Events are delivered to the channels the routes name'
            : 'Nothing is delivered except tests'}>{status.enabled ? 'on' : 'off'}</span
        >
      {/if}
      <span class="spacer"></span>
      <button
        class="btn sm"
        disabled={busy || !kind}
        onclick={() => {
          addOpen = !addOpen;
          routeOpen = false;
        }}><Icon name="plus" size={13} /> Add channel</button
      >
      <button
        class="btn sm"
        disabled={busy || !kind || names.length === 0}
        title={names.length === 0 ? 'Add a channel first' : undefined}
        onclick={() => {
          routeOpen = !routeOpen;
          addOpen = false;
        }}><Icon name="plus" size={13} /> Add route</button
      >
      <button class="btn ghost icon sm" aria-label="Reload notifications" onclick={refresh}
        ><Icon name="refresh" size={13} /></button
      >
    </div>
    <div class="panel-body">
      <p class="faint intro">
        Channels deliver server events such as failed backups by webhook, ntfy or email. Routes say
        which channels each event type goes to. Send a test to check a channel before you turn
        notifications on. Changes apply at once and override the settings file until you reset them.
      </p>

      {#if addOpen}
        <form class="add" onsubmit={addChannel} aria-label="Add a channel">
          <label class="field"
            >Name <input class="input mono" bind:value={add.name} placeholder="ops" /></label
          >
          <label class="field"
            >Type
            <select class="select" bind:value={add.type}>
              {#each CHANNEL_TYPES as t (t)}<option value={t}>{t}</option>{/each}
            </select>
          </label>
          {#if add.type === 'webhook'}
            <label class="field grow"
              >URL <input
                class="input mono"
                bind:value={add.url}
                placeholder="https://hooks.example.org/sparkles"
              /></label
            >
          {:else if add.type === 'ntfy'}
            <label class="field grow"
              >Server <input
                class="input mono"
                bind:value={add.server}
                placeholder="https://ntfy.sh"
              /></label
            >
            <label class="field"
              >Topic <input
                class="input mono"
                bind:value={add.topic}
                placeholder="sparkles-ops"
              /></label
            >
          {:else}
            <label class="field grow"
              >SMTP host <input
                class="input mono"
                bind:value={add.host}
                placeholder="smtp.example.org"
              /></label
            >
            <label class="field"
              >TLS
              <select class="select" bind:value={add.tls}>
                <option value="starttls">STARTTLS (587)</option>
                <option value="tls">TLS (465)</option>
              </select>
            </label>
            <label class="field"
              >From <input
                class="input mono"
                bind:value={add.from}
                placeholder="sparkles@example.org"
              /></label
            >
            <label class="field grow"
              >To <input
                class="input mono"
                bind:value={add.to}
                placeholder="ops@example.org, oncall@example.org"
              /></label
            >
            <label class="field"
              >User name <input
                class="input mono"
                bind:value={add.username}
                placeholder="optional"
              /></label
            >
          {/if}
          <label class="field"
            >{secretLabel(add.type)}
            <input class="input mono" bind:value={add.secret} placeholder="optional" /></label
          >
          <div class="add-actions">
            <button type="button" class="btn sm" onclick={() => (addOpen = false)}>Cancel</button>
            <button class="btn primary sm" type="submit" disabled={busy}>Add</button>
          </div>
          {#if addError}<p class="bad small">{addError}</p>{/if}
          <p class="faint small">
            The secret fields take the name of a secret, never its value. Set the value under the
            channel's fields below or in the Models section's API keys. After you add the channel,
            add a route to it and send a test.
          </p>
        </form>
      {/if}

      {#if routeOpen}
        <form class="add" onsubmit={addRoute} aria-label="Add a route">
          <label class="field grow"
            >Event type <input
              class="input mono"
              bind:value={routeKey}
              list="notify-event-types"
              placeholder="backup.*"
            /></label
          >
          <datalist id="notify-event-types">
            {#each EVENT_TYPES as t (t)}<option value={t}></option>{/each}
          </datalist>
          <fieldset class="field channels-pick">
            <legend>Channels</legend>
            {#each names as n (n)}
              <label class="check"
                ><input
                  type="checkbox"
                  checked={routeChannels.includes(n)}
                  onchange={(e) => toggleRouteChannel(n, e.currentTarget.checked)}
                />
                <span class="mono">{n}</span></label
              >
            {/each}
          </fieldset>
          <div class="add-actions">
            <button type="button" class="btn sm" onclick={() => (routeOpen = false)}>Cancel</button>
            <button class="btn primary sm" type="submit" disabled={busy}>Add</button>
          </div>
          {#if routeError}<p class="bad small">{routeError}</p>{/if}
          <p class="faint small">
            An event type, a prefix ending in .* or * for every event. An event goes to the channels
            of every route that matches it, each once.
          </p>
        </form>
      {/if}

      {#if statusError}
        <div class="error-box">
          <strong>Could not load the notification status.</strong>
          {statusError}
        </div>
      {:else if !status}
        <p class="faint"><span class="spinner"></span> Loading…</p>
      {:else if status.channels.length === 0}
        <p class="faint">
          No channels. Add one, or declare them in the settings file's server.notifications.
        </p>
      {:else}
        <div class="scroll-x">
          <table class="data" aria-label="Notification channels">
            <thead>
              <tr
                ><th>Channel</th><th>Type</th><th>Target</th><th>Routes</th><th>Delivered</th><th
                  >Queue</th
                ><th>Last delivery</th><th></th></tr
              >
            </thead>
            <tbody>
              {#each status.channels as c (c.name)}
                {@const last = lastOf(c)}
                {@const via = routesTo(routes, c.name)}
                <tr data-channel={c.name}>
                  <td class="mono">{c.name}</td>
                  <td class="muted">{c.type}</td>
                  <td class="mono small target">
                    {c.target}
                    {#each c.secrets as s (s)}
                      {@const info = secretOf(s)}
                      <span
                        class="badge {info && info.source === 'missing' ? 'danger' : ''}"
                        title={info ? secretState(info).title : 'A secret this channel names'}
                        ><Icon name="lock" size={10} />
                        {s}{info && info.source === 'missing' ? ' not set' : ''}</span
                      >
                    {/each}
                    {#if tested[c.name]}
                      <div class="test-result small" class:bad={!tested[c.name].ok} role="status">
                        Test: {tested[c.name].text}
                      </div>
                    {/if}
                  </td>
                  <td class="small">
                    {#each via as r, i (r)}{i ? ', ' : ''}<span class="mono">{r}</span>{:else}<span
                        class="faint"
                        title="No route sends events to this channel">none</span
                      >{/each}
                  </td>
                  <td class="small">
                    {c.sent} ok{#if c.failed}, <span class="bad">{c.failed} failed</span>{/if}
                  </td>
                  <td class="small" title="Deliveries waiting, out of the channel's capacity">
                    {c.queued} / {c.queueCapacity}{#if c.inFlight}
                      <span class="badge">sending</span>{/if}
                  </td>
                  <td class="small">
                    {#if last}
                      <span class="badge {last.ok ? 'ok' : 'danger'}"
                        >{last.ok ? 'ok' : 'failed'}</span
                      >
                      <span class="faint">{fmtTime(last.d.at)}</span>
                      {#if !last.ok && outcomeText(last.d)}
                        <div class="bad small err">{outcomeText(last.d)}</div>
                      {/if}
                    {:else}
                      <span class="faint">—</span>
                    {/if}
                  </td>
                  <td class="actions">
                    <button
                      class="btn sm"
                      disabled={testing[c.name]}
                      onclick={() => sendTest(c.name)}
                      aria-label="Send a test to {c.name}"
                      >{#if testing[c.name]}<span class="spinner"></span>{/if} Send test</button
                    >
                    <button
                      class="btn ghost icon sm"
                      disabled={busy || channelLocked(c.name)}
                      title={channelLocked(c.name)
                        ? 'This channel, or some of its fields, is locked in the settings file'
                        : 'Remove this channel'}
                      onclick={() => (removeChannel = c.name)}
                      aria-label="Remove channel {c.name}"><Icon name="trash" size={12} /></button
                    >
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}

      {#if removed.length}
        <div class="removed">
          {#each removed as r (r.path)}
            <div class="removed-row" data-removed={r.name}>
              <Icon name="alert" size={13} />
              <span
                >Channel <span class="mono">{r.name}</span> from the settings file is removed here.</span
              >
              <span class="spacer"></span>
              <button
                class="btn sm"
                disabled={busy}
                onclick={() => restoreChannel(r.path, r.name)}
                aria-label="Use server config for channel {r.name}">Use server config</button
              >
            </div>
          {/each}
        </div>
      {/if}

      {#if kind}
        <h3 class="sub">Routes</h3>
        {#if Object.keys(routes).length === 0}
          <p class="faint small">No routes, so events go nowhere. Add a route to a channel.</p>
        {:else}
          <div class="scroll-x">
            <table class="data" aria-label="Notification routes">
              <thead><tr><th>Event type</th><th>Channels</th><th></th></tr></thead>
              <tbody>
                {#each Object.entries(routes).sort( ([a], [b]) => a.localeCompare(b) ) as [key, chans] (key)}
                  {@const declared = routeDeclared(kind, key)}
                  <tr data-route={key}>
                    <td class="mono">{key}</td>
                    <td class="small">
                      {#each chans as ch, i (ch)}{i ? ', ' : ''}<span class="mono">{ch}</span
                        >{:else}<span class="faint">none</span>{/each}
                    </td>
                    <td class="actions">
                      <button
                        class="btn ghost icon sm"
                        disabled={busy || declared || routeLocked(key)}
                        title={declared
                          ? 'The settings file declares this route. Empty its channels below instead.'
                          : 'Remove this route'}
                        onclick={() => (removeRoute = key)}
                        aria-label="Remove the route for {key}"
                        ><Icon name="trash" size={12} /></button
                      >
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        {/if}
      {/if}
    </div>
  </div>

  <div class="panel" data-testid="notification-deliveries">
    <div class="panel-head">
      <h2>Recent deliveries</h2>
      {#if status && status.queued > 0}<span class="badge">{status.queued} queued</span>{/if}
    </div>
    <div class="panel-body">
      {#if status && status.active.length}
        <h3 class="sub first">Active conditions</h3>
        <p class="faint small">
          Lasting problems that were notified and have not cleared. Each is notified again at most
          once per repeat interval until it clears.
        </p>
        <div class="scroll-x">
          <table class="data" aria-label="Active conditions">
            <thead
              ><tr
                ><th>Event</th><th>Dataset</th><th>Since</th><th>Last notified</th><th>Count</th
                ></tr
              ></thead
            >
            <tbody>
              {#each status.active as a (a.key)}
                <tr data-condition={a.key}>
                  <td class="mono" title={a.key}>{a.event}</td>
                  <td class="mono small">{a.dataset ?? '—'}</td>
                  <td class="small">{fmtTime(a.first)}</td>
                  <td class="small">{fmtTime(a.last)}</td>
                  <td class="small">{a.count}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
      {#if status && status.active.length}<h3 class="sub">Deliveries</h3>{/if}
      {#if !status}
        <p class="faint">—</p>
      {:else if recent.length === 0}
        <p class="faint">No deliveries since the server started.</p>
      {:else}
        <div class="scroll-x">
          <table class="data" aria-label="Recent deliveries">
            <thead
              ><tr
                ><th>Time</th><th>Event</th><th>Channel</th><th>Result</th><th>Attempts</th><th
                  >Detail</th
                ></tr
              ></thead
            >
            <tbody>
              {#each recent as d, i (`${d.id}:${d.channel}:${i}`)}
                <tr data-delivery={d.id}>
                  <td class="small">{fmtTime(d.at)}</td>
                  <td class="mono small">{d.event}</td>
                  <td class="mono small">{d.channel}</td>
                  <td><span class="badge {resultClass(d.result)}">{d.result}</span></td>
                  <td class="small">{d.attempts ?? '—'}</td>
                  <td class="small" class:bad={d.result !== 'ok'}>{outcomeText(d)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
        <p class="faint small">
          The last 50 deliveries since the server started. The counts and the queue live in the
          process and start empty after a restart.
        </p>
      {/if}
    </div>
  </div>

  {#if kindError}
    <div class="error-box">
      <strong>Could not load the notification configuration.</strong>
      {kindError}
    </div>
  {:else if kind}
    {#key fieldsKey}
      <SettingsKindPanel
        title="Notification configuration"
        text="Whether notifications are on, each channel's fields, the channels of each route and the retries. Each field shows whether it comes from the settings file or was changed here."
        url={NOTIFICATIONS_URL}
        {fields}
        canEdit
        declaredName="the settings file"
        forbiddenText="Changing notifications requires the server-admin permission on a server that is not read-only."
        onchange={onKind}
        check={checkRoutes}
        {extras}
        secrets={secretFields}
      >
        {#snippet groupExtra(group: string)}
          {@const name = groupChannel(group)}
          {#if name}
            {@const lock = channelLocked(name)}
            <div class="group-actions">
              <span class="spacer"></span>
              <button
                type="button"
                class="btn sm"
                disabled={testing[name]}
                onclick={() => sendTest(name)}>Send test</button
              >
              <button
                type="button"
                class="btn danger sm"
                disabled={busy || lock}
                title={lock
                  ? 'This channel, or some of its fields, is locked in the settings file'
                  : 'Remove this channel'}
                onclick={() => (removeChannel = name)}
                ><Icon name="trash" size={12} /> Remove</button
              >
            </div>
          {:else if group === ROUTES_GROUP}
            <p class="faint small group-note">
              A route's channels must exist. Add or remove whole routes in the Notifications panel
              above.
            </p>
          {/if}
        {/snippet}
      </SettingsKindPanel>
    {/key}
  {/if}
</section>

<Modal
  open={removeChannel != null}
  title="Remove channel {removeChannel ?? ''}?"
  onclose={() => (removeChannel = null)}
>
  <p>
    Events stop going to this channel, and deliveries waiting in its queue are dropped. If it comes
    from the settings file, it stays removed until you choose Use server config.
  </p>
  {#if removeChannel && routesTo(routes, removeChannel).length}
    <p>
      It is also taken out of the routes
      <span class="mono">{routesTo(routes, removeChannel).join(', ')}</span>.
    </p>
  {/if}
  {#snippet actions()}
    <button class="btn" onclick={() => (removeChannel = null)}>Cancel</button>
    <button class="btn danger solid" disabled={busy} onclick={confirmRemoveChannel}>
      {#if busy}<span class="spinner"></span>{/if} Remove
    </button>
  {/snippet}
</Modal>

<Modal
  open={removeRoute != null}
  title="Remove the route for {removeRoute ?? ''}?"
  onclose={() => (removeRoute = null)}
>
  <p>Events of this type stop going to the route's channels, unless another route matches them.</p>
  {#snippet actions()}
    <button class="btn" onclick={() => (removeRoute = null)}>Cancel</button>
    <button class="btn danger solid" disabled={busy} onclick={confirmRemoveRoute}>
      {#if busy}<span class="spinner"></span>{/if} Remove
    </button>
  {/snippet}
</Modal>

<style>
  .notify {
    display: grid;
    gap: 16px;
    min-width: 0;
  }
  .notify > :global(*) {
    min-width: 0;
  }
  .intro {
    max-width: 80ch;
    margin: 0 0 10px;
    font-size: var(--fs-sm);
  }
  .add {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: 8px 12px;
    margin: 0 0 12px;
    padding: 10px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface-2);
  }
  .add .field {
    display: grid;
    gap: 4px;
    font-size: var(--fs-sm);
    color: var(--text-2);
    min-width: 0;
  }
  .add .grow {
    flex: 1 1 240px;
  }
  .add-actions {
    display: flex;
    gap: 6px;
  }
  .add p {
    flex-basis: 100%;
    margin: 0;
  }
  .channels-pick {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
    border: 0;
    margin: 0;
    padding: 0;
  }
  .channels-pick legend {
    padding: 0;
    margin-bottom: 4px;
  }
  .check {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  .scroll-x {
    overflow-x: auto;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .bad {
    color: var(--danger);
  }
  .target {
    max-width: 36ch;
    overflow-wrap: anywhere;
  }
  .target .badge {
    margin-left: 4px;
  }
  .test-result {
    margin-top: 4px;
    font-family: var(--font-ui);
  }
  .err {
    max-width: 40ch;
    overflow-wrap: anywhere;
  }
  .actions {
    white-space: nowrap;
    text-align: right;
  }
  .sub {
    margin: 16px 0 6px;
    font-size: var(--fs-md, 1rem);
  }
  .sub.first {
    margin-top: 0;
  }
  .removed {
    display: grid;
    gap: 6px;
    margin-top: 10px;
  }
  .removed-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 8px;
    padding: 6px 10px;
    border-radius: var(--r);
    border: 1px solid color-mix(in srgb, var(--warn) 40%, transparent);
    font-size: var(--fs-sm);
  }
  .removed-row :global(svg) {
    color: var(--warn);
  }
  .group-actions {
    display: flex;
    gap: 6px;
    padding: 2px 10px 6px;
  }
  .group-note {
    margin: 0;
    padding: 2px 10px 6px;
  }
</style>
