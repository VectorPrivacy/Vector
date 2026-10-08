<script>
    // The outproxies clearnet servers are reached through, tried top to bottom: reorder them,
    // switch one off, remove your own, add one, or go back to the built-in list. Every change
    // saves the whole list; a refusal leaves it as it was and says why.
    import { tick } from 'svelte';
    import { transportState } from '../lib/transport.svelte.js';
    import InfoIcon from './InfoIcon.svelte';

    let { h } = $props();   // h: TransportHandlers (js/transport.js)

    const MAX = 8;
    const t = transportState();
    const cfg = $derived(t.config);
    const list = $derived(cfg?.outproxies || []);
    // Health is the running instance's; nothing is known about an outproxy until I2P is in use.
    const health = $derived(t.view?.kind === 'i2p' ? (t.view.detail?.outproxies || {}) : null);
    const stale = $derived(!t.view?.ready);

    let busy = $state(false);
    let error = $state('');
    let adding = $state(false);
    let name = $state('');
    let address = $state('');
    let port = $state('');

    const input = (o) => ({ name: o.name, address: o.address, port: o.port, enabled: o.enabled });
    async function save(next) {
        busy = true;
        error = '';
        try { await h.i2p.saveOutproxies(next); return true; }
        catch (e) { error = String(e); return false; }
        finally { busy = false; }
    }
    // Held for the reorder gesture: the moved row's arrow keeps the keyboard focus.
    let listEl = $state(null);
    async function move(i, by) {
        const id = list[i].id;
        const next = list.map(input);
        const [o] = next.splice(i, 1);
        next.splice(i + by, 0, o);
        if (!(await save(next))) return;
        await tick();
        const row = [...(listEl?.children || [])].find((li) => li.dataset.id === id);
        const arrows = row ? [...row.querySelectorAll('.i2p-op-move')] : [];
        // The same direction while it can still go that way, else the other one.
        const want = by < 0 ? arrows[0] : arrows[1];
        (want && !want.disabled ? want : arrows.find((b) => !b.disabled))?.focus();
    }
    async function toggle(i, e) {
        const box = e.currentTarget;
        await save(list.map((o, j) => ({ ...input(o), enabled: j === i ? box.checked : o.enabled })));
        box.checked = !!list[i]?.enabled;
    }
    async function remove(i) {
        if (await h.i2p.confirmRemove(list[i].name)) save(list.filter((_, j) => j !== i).map(input));
    }
    async function reset() {
        if (await h.i2p.confirmReset(list.some((o) => !o.builtin))) save(null);
    }
    async function add() {
        const entry = { name: name.trim(), address: address.trim().toLowerCase(), port: port.trim() ? Number(port) : 80, enabled: true };
        if (await save([...list.map(input), entry])) closeForm();
    }
    function closeForm() { adding = false; name = ''; address = ''; port = ''; error = ''; }

    /** A b32 address as `5d4s…xbwa:80`; a name in full (the row ellipsizes it), the address in the tooltip. */
    function short(o) {
        const b32 = /^([a-z2-7]{52,})\.b32\.i2p$/i.exec(o.address);
        const head = b32 ? `${b32[1].slice(0, 4)}…${b32[1].slice(-4)}` : o.address;
        return `${head}:${o.port}`;
    }
    /** The row's word, and why when it isn't simply working (`why` is the outproxy's last error). */
    function healthOf(o) {
        const s = health?.[o.id];
        if (!s) return { cls: 'unknown', text: 'Not tried', why: null };
        const why = s.last_error || null;
        const ports = s.refused_ports || [];
        if (s.state === 'cooling') return { cls: 'cooling', text: `Resting ${Math.max(1, Math.ceil((s.cooling_for || 60) / 60))}m`, why };
        if (s.state === 'failing') return { cls: 'failing', text: 'Failing', why };
        if (s.state === 'ok') return { cls: 'ok', text: ports.length ? `OK, not port ${ports.join(', ')}` : 'OK', why: null };
        if (ports.length) return { cls: 'refusing', text: `Refuses port ${ports.join(', ')}`, why: null };
        return why ? { cls: 'unknown', text: 'Not reached yet', why } : { cls: 'unknown', text: 'Not tried', why: null };
    }
    function healthTitle(s) {
        return [s.why, stale ? "Last known. I2P isn't connected." : null].filter(Boolean).join(' ') || null;
    }
    const onEnter = (e) => { if (e.key === 'Enter') { e.preventDefault(); add(); } };
</script>

<div class="i2p-ops">
    <div class="st-row i2p-ops-head">
        <span class="st-row-label">Outproxies<InfoIcon onclick={() => h.help('i2pOutproxy')} /></span>
        {#if cfg?.customized}
            <button type="button" class="net-link" disabled={busy} onclick={reset}>Reset</button>
        {/if}
    </div>
    <ol class="i2p-op-list" class:busy bind:this={listEl}>
        {#each list as o, i (o.id)}
            {@const s = health ? healthOf(o) : null}
            <li class="i2p-op" class:off={!o.enabled} data-id={o.id}>
                <span class="i2p-op-order">
                    <button type="button" class="i2p-op-move" aria-label="Move {o.name} up" title="Move up" disabled={busy || i === 0} onclick={() => move(i, -1)}>
                        <svg viewBox="0 0 12 8" aria-hidden="true"><path d="M1.5 6.5 6 2l4.5 4.5"/></svg>
                    </button>
                    <button type="button" class="i2p-op-move" aria-label="Move {o.name} down" title="Move down" disabled={busy || i === list.length - 1} onclick={() => move(i, 1)}>
                        <svg viewBox="0 0 12 8" aria-hidden="true"><path d="M1.5 1.5 6 6l4.5-4.5"/></svg>
                    </button>
                </span>
                <span class="i2p-op-rank">{i + 1}</span>
                <span class="i2p-op-name">{o.name}</span>
                <span class="i2p-op-addr" title="{o.address}:{o.port}">{short(o)}</span>
                {#if s}<span class="i2p-op-health {s.cls}" class:stale title={healthTitle(s)}><span class="i2p-op-dot"></span>{s.text}</span>{/if}
                {#if s?.why && !stale}<span class="i2p-op-why">{s.why}</span>{/if}
                {#if !o.builtin}
                    <button type="button" class="i2p-op-remove" aria-label="Remove {o.name}" title="Remove" disabled={busy} onclick={() => remove(i)}>
                        <svg viewBox="0 0 12 12" aria-hidden="true"><path d="M2.5 2.5l7 7M9.5 2.5l-7 7"/></svg>
                    </button>
                {/if}
                <label class="toggle-container i2p-op-toggle" title={o.enabled ? 'On' : 'Off'}>
                    <input type="checkbox" aria-label="Use {o.name}" checked={o.enabled} disabled={busy} onchange={(e) => toggle(i, e)}>
                    <span class="neon-toggle"></span>
                </label>
            </li>
        {/each}
    </ol>

    {#if adding}
        <div class="i2p-form i2p-op-form">
            <div class="i2p-fields">
                <label class="i2p-field">
                    <span>Name</span>
                    <!-- svelte-ignore a11y_autofocus -->
                    <input type="text" maxlength="32" autocomplete="off" spellcheck="false" autofocus bind:value={name} onkeydown={onEnter}>
                </label>
                <label class="i2p-field i2p-field-wide">
                    <span>Address</span>
                    <input type="text" class="mono" placeholder="xxxx.b32.i2p" autocomplete="off" autocapitalize="none" spellcheck="false" bind:value={address} onkeydown={onEnter}>
                </label>
                <label class="i2p-field i2p-field-port">
                    <span>Port</span>
                    <input type="text" class="mono" placeholder="80" inputmode="numeric" maxlength="5" autocomplete="off"
                           bind:value={port} oninput={() => { port = port.replace(/\D/g, ''); }} onkeydown={onEnter}>
                </label>
            </div>
            <div class="i2p-form-actions">
                <button type="button" class="st-btn" onclick={closeForm}>Cancel</button>
                <button type="button" class="st-btn net-primary" disabled={busy || !name.trim() || !address.trim()} onclick={add}>Save</button>
            </div>
        </div>
    {:else if list.length < MAX}
        <button type="button" class="st-btn i2p-op-add" disabled={busy} onclick={() => { adding = true; error = ''; }}>
            <svg viewBox="0 0 12 12" aria-hidden="true"><path d="M6 1.5v9M1.5 6h9"/></svg>
            Add Outproxy
        </button>
    {:else}
        <p class="i2p-note i2p-muted">You can add up to {MAX} outproxies.</p>
    {/if}
    {#if error}<p class="i2p-note error" role="alert">{error}</p>{/if}
</div>
