<script>
    // A server's address inside Tor or I2P: on that network Vector reaches the server there instead
    // of through an exit. TLS still runs end to end with the server, so only port 443 uses it.
    import { transportState } from '../../lib/transport.svelte.js';
    import InfoIcon from '../InfoIcon.svelte';

    // url: the server's full URL. relay: one of the user's own relays, which may list its address.
    // kind: the network this field is for, 'tor' or 'i2p'.
    let { url = '', relay = false, kind = 'i2p', h } = $props();   // h: TransportAliasHandlers (js/transport.js)

    const NET = { tor: { label: 'Tor', title: 'Onion Address', noun: 'onion address', suffix: '.onion', placeholder: 'xxxx.onion', help: 'onionAlias' },
                  i2p: { label: 'I2P', title: 'I2P Address', noun: 'I2P address', suffix: '.i2p', placeholder: 'xxxx.b32.i2p', help: 'i2pAlias' } };
    const net = $derived(NET[kind] || NET.i2p);

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
    // A server already inside the network needs no address there; a build without it has nothing to use one with.
    const offered = $derived(!!target && !!t.view?.supported?.includes(kind) && !target.host.endsWith(net.suffix));
    const entry = $derived(target ? t.aliases.find((a) => a.host === target.host) || null : null);
    const saved = $derived(entry?.twins?.[kind] || '');
    // Checks and lookups run through that network only, so they wait until it is connected. A
    // lookup asks the relay itself over clearnet, which I2P-Only rules out.
    const ready = $derived(t.view?.kind === kind && !!t.view.ready);
    const canFind = $derived(relay && (kind !== 'i2p' || t.config?.exit !== 'off'));

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
        await h.save(target.host, kind, value.trim().toLowerCase() || null);
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
    const remove = () => run(async () => { await h.save(target.host, kind, null); editing = false; });
    // A twin carries TLS on 443 only: on any other port an address would never be used.
    const usable = $derived(!!target && target.port === 443);

    const line = $derived.by(() => {
        if (!target) return null;
        if (target.port !== 443) {
            return { text: saved ? "This server doesn't use port 443, so this address isn't used." : `This server doesn't use port 443, so it can't use an ${net.noun}.`, tone: 'muted' };
        }
        if (!saved) return null;
        const c = entry.check || {};
        if (c.state === 'ok') return { text: c.text || `Checked. It serves ${target.host}.`, tone: 'ok' };
        if (c.state === 'failed') return { text: c.text || `This address doesn't serve ${target.host}. Vector won't use it.`, tone: 'error' };
        return { text: c.text || `Not checked yet. Vector checks it when ${net.label} connects.`, tone: 'muted' };
    });
</script>

{#if offered}
    <div class="relay-connection alias-field alias-{kind}">
        <h4>{net.title}<InfoIcon onclick={() => h.help(net.help)} /></h4>
        {#if usable}
            <div class="alias-row">
                <input type="text" class="relay-form-input alias-input" placeholder={net.placeholder} aria-label={net.title}
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
