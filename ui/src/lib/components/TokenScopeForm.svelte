<script lang="ts">
  // Name, lifetime and scope of a new token: "All my access", or a level per dataset
  // (at most the user's own) and the server permissions the user holds.
  import {
    ALL_ACCESS,
    LEVELS,
    SERVER_PERMS,
    levelAtLeast,
    scopeFrom,
    ttlOptions,
    type Level,
    type Scope,
    type ServerPerm,
    type Whoami,
  } from '$lib/auth';

  let {
    who,
    name = $bindable('token'),
    expiresIn = $bindable('30d'),
    scope = $bindable<Scope>(ALL_ACCESS),
  }: { who: Whoami | null; name?: string; expiresIn?: string; scope?: Scope } = $props();

  let preset = $state<'all' | 'custom'>('all');
  let levels = $state<Record<string, Level | ''>>({});
  let perms = $state<ServerPerm[]>([]);

  const admin = $derived(who?.server.includes('server-admin') ?? false);
  const datasets = $derived(
    Object.entries(who?.datasets ?? {}).sort(([a], [b]) => a.localeCompare(b)),
  );
  const heldPerms = $derived(SERVER_PERMS.filter((p) => admin || who?.server.includes(p)));
  const ttls = $derived(ttlOptions(who?.tokensPolicy?.maxTtlSeconds));

  $effect(() => {
    if (!ttls.some((t) => t.value === expiresIn)) expiresIn = ttls[ttls.length - 1].value;
  });

  $effect(() => {
    scope = preset === 'all' ? ALL_ACCESS : scopeFrom(who, levels, perms);
  });

  function togglePerm(p: ServerPerm, on: boolean) {
    perms = on ? [...perms, p] : perms.filter((x) => x !== p);
  }
</script>

<div class="scope-form">
  <label class="field"
    >Token name
    <input class="input" bind:value={name} maxlength="80" required />
  </label>

  <label class="field"
    >Expires after
    <select class="select" bind:value={expiresIn}>
      {#each ttls as t (t.value)}<option value={t.value}>{t.label}</option>{/each}
    </select>
  </label>

  <fieldset>
    <legend>Access</legend>
    <label class="radio">
      <input type="radio" bind:group={preset} value="all" />
      <span
        ><strong>All my access</strong> <span class="faint">— follows your permissions</span></span
      >
    </label>
    <label class="radio">
      <input type="radio" bind:group={preset} value="custom" />
      <span><strong>Chosen datasets</strong></span>
    </label>
    {#if preset === 'custom'}
      <div class="grid">
        {#each datasets as [ds, mine] (ds)}
          <span class="mono ds">{ds}</span>
          <select class="select sm" bind:value={levels[ds]} aria-label="Level on {ds}">
            <option value="">none</option>
            {#each LEVELS.filter((l) => admin || levelAtLeast(mine, l)) as l (l)}
              <option value={l}>{l}</option>
            {/each}
          </select>
        {:else}
          <span class="faint">You have no datasets.</span>
        {/each}
      </div>
      {#if heldPerms.length}
        <div class="perms">
          {#each heldPerms as p (p)}
            <label class="check">
              <input
                type="checkbox"
                checked={perms.includes(p)}
                onchange={(e) => togglePerm(p, e.currentTarget.checked)}
              />
              <span class="mono">{p}</span>
            </label>
          {/each}
        </div>
      {/if}
    {/if}
  </fieldset>
</div>

<style>
  .scope-form {
    display: grid;
    gap: 12px;
  }
  fieldset {
    margin: 0;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    display: grid;
    gap: 6px;
  }
  legend {
    padding: 0 4px;
    font-size: var(--fs-sm);
    color: var(--text-2);
    font-weight: 500;
  }
  .radio,
  .check {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: var(--fs-sm);
  }
  .grid {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 6px 12px;
    align-items: center;
    padding: 6px 0 2px 22px;
    max-height: 220px;
    overflow: auto;
  }
  .ds {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .select.sm {
    height: 24px;
  }
  .perms {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
    padding: 4px 0 0 22px;
  }
</style>
