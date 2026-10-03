<script>
    // Your Keys: the seed phrase as a numbered grid and the nsec, each blurred until tapped,
    // each with its own copy. The saving variant closes only on its button.
    import { keysModal } from '../lib/keys.svelte.js';
    import { popIn } from '../lib/popin.js';
    let { h } = $props();   // h: copy(text), close(), transfer(), proceed()
    const st = keysModal.state();

    let shown = $state({ seed: false, nsec: false });
    let copied = $state('');
    let copyTimer = 0;
    $effect(() => { st.tick; shown = { seed: false, nsec: false }; copied = ''; });

    const words = $derived(st.seed ? st.seed.trim().split(/\s+/) : []);

    function copy(which) {
        h.copy(which === 'seed' ? st.seed : st.nsec).then(() => {
            copied = which;
            clearTimeout(copyTimer);
            copyTimer = setTimeout(() => { copied = ''; }, 1600);
        }, () => {});
    }
    function dismiss() { if (!st.saving) h.close(); }
</script>

<svelte:window onkeydown={(e) => { if (st.active && !st.closing && e.key === 'Escape') dismiss(); }} />

{#snippet secretHead(which, label)}
    <div class="keys-row-head">
        <span class="vcard-label">{label}</span>
        <button type="button" class="keys-copy" class:done={copied === which} onclick={() => copy(which)}>
            <span class="keys-copy-glyph" aria-hidden="true"></span>{copied === which ? 'Copied' : 'Copy'}
        </button>
    </div>
{/snippet}

{#snippet veil(which)}
    {#if !shown[which]}
        <button type="button" class="keys-veil" onclick={() => { shown[which] = true; }}>
            <span class="keys-veil-glyph" aria-hidden="true"></span>Tap to reveal
        </button>
    {/if}
{/snippet}

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="vcard-overlay" class:active={st.active} class:closing={st.closing}
     onclick={(e) => { if (e.target === e.currentTarget) dismiss(); }}>
    <div class="vcard keys-card" role="dialog" aria-modal="true" aria-labelledby="keys-title" use:popIn={st.tick}>
        <div class="vcard-head">
            <span class="vcard-badge keys-badge" aria-hidden="true"></span>
            <h3 id="keys-title" class="vcard-title">{st.stage === 'warn' ? 'Show Private Keys?' : st.saving ? 'Save Your Keys' : 'Your Keys'}</h3>
            {#if !st.saving}
                <button type="button" class="vcard-x" aria-label="Close" onclick={() => h.close()}>&#x2715;</button>
            {/if}
        </div>
        {#if st.stage === 'warn'}
            <p class="vcard-lead">
                To use Vector on another device, use <span class="keys-warn">Sign in on Another Device</span> instead.
                It's quicker, and your keys never leave this device in a form anyone else can read.
            </p>
            <p class="vcard-lead">
                Only show your keys to back them up offline or to use them in another Nostr app. Anyone who
                sees them can read your messages and act as you.
            </p>
            <div class="vcard-actions">
                <button type="button" class="vcard-btn" onclick={() => h.transfer()}>Sign in on Another Device</button>
                <button type="button" class="vcard-btn ghost" onclick={() => h.proceed()}>Show Keys Anyway</button>
            </div>
        {:else}
            <p class="vcard-lead">
                {st.saving
                    ? 'This browser keeps nothing once it closes. These keys are the only way back into your account.'
                    : 'Your keys are your account. Anyone who has them can read your messages and speak as you.'}
                <span class="keys-warn">Write them down offline and never share them.</span>
            </p>

            {#if words.length}
                <section class="keys-row">
                    {@render secretHead('seed', 'Seed Phrase')}
                    <div class="keys-box" class:veiled={!shown.seed}>
                        <ol class="keys-words" aria-hidden={!shown.seed}>
                            {#each words as word, i}
                                <li><span class="keys-n">{i + 1}</span>{word}</li>
                            {/each}
                        </ol>
                        {@render veil('seed')}
                    </div>
                </section>
            {/if}

            <section class="keys-row">
                {@render secretHead('nsec', 'Private Key')}
                <div class="keys-box" class:veiled={!shown.nsec}>
                    <p class="keys-nsec" aria-hidden={!shown.nsec}>{st.nsec}</p>
                    {@render veil('nsec')}
                </div>
            </section>

            <div class="vcard-actions">
                <button type="button" class="vcard-btn" onclick={() => h.close()}>{st.saving ? "I've Saved Them" : 'Done'}</button>
                {#if !st.saving}
                    <button type="button" class="xfer-link" onclick={() => h.transfer()}>Moving to another device? Sign in there with a code instead</button>
                {/if}
            </div>
        {/if}
    </div>
</div>
