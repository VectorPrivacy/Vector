<script>
    // A server's I2P address: in I2P mode Vector reaches the server there, inside I2P, instead of
    // through an outproxy. TLS still runs end to end with the server, so only port 443 uses it.
    import { transportState } from '../../lib/transport.svelte.js';
    import InfoIcon from '../InfoIcon.svelte';

    // url: the server's full URL. relay: one of the user's own relays, which may list its address.
    let { url = '', relay = false, h } = $props();   // h: { help(key), save(host, address), find(url), check(host) }

    const t = transportState();
    const target = $derived.by(() => {
        try {
            const u = new URL(url);
            const tls = u.protocol === 'wss:' || u.protocol === 'https:';
            return { host: u.hostname.toLowerCase().replace(/\.$/, ''), port: u.port ? Number(u.port) : tls ? 443 : 80 };
        } catch {
            return null;
        }
    });
    // An .i2p server is already inside I2P; a build without I2P has nothing to use it with.
    const offered = $derived(!!target && !!t.view?.supported?.includes('i2p') && !target.host.endsWith('.i2p'));
    const entry = $derived(target ? t.aliases.find((a) => a.host === target.host) || null : null);
    const saved = $derived(entry?.twins?.i2p || '');
    // Checks and lookups run inside I2P only, so they wait until it is connected. A lookup asks
    // the relay itself over clearnet, which I2P-Only rules out.
    const ready = $derived(t.view?.kind === 'i2p' && !!t.view.ready);
    const canFind = $derived(relay && t.config?.exit !== 'off');

    let value = $state('');
    let editing = false;
    $effect(() => { const s = saved; if (!editing) value = s; });
    const dirty = $derived(value.trim().toLowerCase() !== saved);

    let busy = $state(false);
    let note = $state(null);   // { text, tone }: what the last action said

    async function run(fn) {
        busy = true;
        note = null;
        try { await fn(); } catch (e) { note = { text: String(e), tone: 'error' }; } finally { busy = false; }
    }
    const save = () => run(async () => {
        await h.save(target.host, value.trim().toLowerCase() || null);
        editing = false;
    });
    // A listed address is checked on the spot; it fills the field and the user decides, so the
    // note says it isn't saved yet.
    const find = () => run(async () => {
        const r = await h.find(url);
        if (!r?.found) { note = { text: r?.text || '', tone: 'muted' }; return; }
        value = r.found;
        editing = true;
        const c = r.check;
        note = c?.state === 'ok' ? { text: 'Found and checked. Save to use it.', tone: 'ok' }
            : c?.state === 'failed' ? { text: c.text || `This address doesn't serve ${target.host}. Vector won't use it.`, tone: 'error' }
            : { text: 'Found it. Save to use it.', tone: 'muted' };
    });
    const check = () => run(() => h.check(target.host));
    const remove = () => run(async () => { await h.save(target.host, null); editing = false; });
    // A twin carries TLS on 443 only: on any other port an address would never be used.
    const usable = $derived(!!target && target.port === 443);
    // Most people never use I2P: the field stays one quiet link until I2P is in use, an address
    // is saved, or the user asks for it (setting one up before switching still works).
    let asked = $state(false);
    const open = $derived(t.view?.kind === 'i2p' || !!saved || asked);

    const line = $derived.by(() => {
        if (!target) return null;
        if (target.port !== 443) {
            return { text: saved ? "This server doesn't use port 443, so this address isn't used." : "This server doesn't use port 443, so it can't use an I2P address.", tone: 'muted' };
        }
        if (!saved) return null;
        const c = entry.check || {};
        if (c.state === 'ok') return { text: c.text || `Checked. It serves ${target.host}.`, tone: 'ok' };
        if (c.state === 'failed') return { text: c.text || `This address doesn't serve ${target.host}. Vector won't use it.`, tone: 'error' };
        return { text: c.text || 'Not checked yet. Vector checks it when I2P connects.', tone: 'muted' };
    });
</script>

{#if offered && !open}
    {#if usable}
        <div class="alias-field alias-collapsed">
            <button type="button" class="net-link" onclick={() => { asked = true; }}>Add I2P Address</button>
            <InfoIcon align="middle" onclick={() => h.help('i2pAlias')} />
        </div>
    {/if}
{:else if offered}
    <div class="relay-connection alias-field">
        <h4>I2P Address<InfoIcon onclick={() => h.help('i2pAlias')} /></h4>
        {#if usable}
            <div class="alias-row">
                <input type="text" class="relay-form-input alias-input" placeholder="xxxx.b32.i2p" aria-label="I2P address"
                       autocomplete="off" autocapitalize="none" spellcheck="false" disabled={busy}
                       bind:value onfocus={() => { editing = true; }}
                       onkeydown={(e) => { if (e.key === 'Enter' && dirty) { e.preventDefault(); save(); } }}>
                <button type="button" class="st-btn net-primary" disabled={busy || !dirty} onclick={save}>Save</button>
            </div>
        {:else if saved}
            <div class="alias-row">
                <span class="alias-saved" title={saved}>{saved}</span>
                <button type="button" class="st-btn" disabled={busy} onclick={remove}>Remove</button>
            </div>
        {/if}
        {#if note?.text}<p class="alias-line {note.tone}" role="status">{note.text}</p>{/if}
        {#if line}<p class="alias-line {line.tone}">{line.text}</p>{/if}
        {#if usable && ((ready && canFind) || saved)}
            <div class="alias-actions">
                {#if ready && canFind}<button type="button" class="net-link" disabled={busy} onclick={find}>Find</button>{/if}
                {#if ready && saved}<button type="button" class="net-link" disabled={busy} onclick={check}>Check</button>{/if}
                {#if saved}<button type="button" class="net-link" disabled={busy} onclick={remove}>Remove</button>{/if}
            </div>
        {/if}
    </div>
{/if}
