<script>
    // Make a Channel or a Category: its kind and privacy, then its name, in one card.
    import { createChannelModal, patchCreateChannel } from '../lib/create-channel.svelte.js';
    import { popIn } from '../lib/popin.js';
    let { h } = $props();   // h: submit() (create what the draft describes), close()
    const st = createChannelModal.state();
    const TYPES = [
        { kind: 'text', icon: 'channel-hash', name: 'Text', desc: 'Messages, images, files and threads' },
        { kind: 'voice', icon: 'volume-max', name: 'Voice', desc: 'Talk together, with cameras and screens' },
    ];
    // The protocol's name cap, in bytes.
    const MAX = 64;
    const bytes = $derived(new TextEncoder().encode(st.name).length);
    const valid = $derived(st.name.trim().length > 0 && bytes <= MAX);
    const noun = $derived(st.category ? 'Category' : 'Channel');
    let input = $state(null);
    $effect(() => { st.tick; if (st.active) input?.focus(); });
    function submit() {
        if (valid && !st.busy) h.submit();
    }
</script>

<svelte:window onkeydown={(e) => { if (st.active && !st.closing && e.key === 'Escape') h.close(); }} />

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="ccm-overlay" class:active={st.active} class:closing={st.closing}
     onclick={(e) => { if (e.target === e.currentTarget) h.close(); }}>
    <div class="ccm-card" use:popIn={st.tick} role="dialog" aria-modal="true" aria-labelledby="ccm-title">
        <div class="ccm-head">
            <div class="ccm-heading">
                <h2 id="ccm-title" class="ccm-title">Create {noun}</h2>
                {#if st.categoryName}<span class="ccm-sub cutoff">in {st.categoryName}</span>{/if}
            </div>
            <button class="status-dialog-x" aria-label="Close" onclick={() => h.close()}>&#x2715;</button>
        </div>

        {#if !st.category}
            <span class="ccm-label">Channel Type</span>
            <div class="ccm-types" role="radiogroup" aria-label="Channel Type">
                {#each TYPES as t (t.kind)}
                    <button class="ccm-type" class:selected={st.kind === t.kind} role="radio" aria-checked={st.kind === t.kind}
                            onclick={() => patchCreateChannel({ kind: t.kind })}>
                        <span class="ccm-type-icon"><span class="icon icon-{t.icon}"></span></span>
                        <span class="ccm-type-text">
                            <span class="ccm-type-name">{t.name}</span>
                            <span class="ccm-type-desc">{t.desc}</span>
                        </span>
                        <span class="ccm-radio"></span>
                    </button>
                {/each}
            </div>
        {/if}

        <label class="ccm-label" for="ccm-name">{noun} Name</label>
        <div class="ccm-name" class:ccm-name-plain={st.category}>
            {#if !st.category}
                <span class="ccm-name-icon"><span class="icon" class:icon-channel-hash={st.kind === 'text'} class:icon-volume-max={st.kind === 'voice'}></span></span>
            {/if}
            <input id="ccm-name" class="cs-input" bind:this={input} value={st.name} autocomplete="off" spellcheck="false"
                   placeholder={st.category ? 'New Category' : st.kind === 'voice' ? 'Lounge' : 'new-channel'}
                   oninput={(e) => patchCreateChannel({ name: e.currentTarget.value, error: '' })}
                   onkeydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); submit(); } }}>
            {#if bytes > MAX - 16}<span class="cs-count ccm-count" class:warn={bytes > MAX}>{MAX - bytes}</span>{/if}
        </div>

        {#if !st.category}
            <label class="toggle-container cs-perm ccm-private">
                <span class="cs-perm-text">
                    <span class="cs-perm-name"><span class="icon icon-locked"></span>Private Channel</span>
                    <span class="cs-perm-desc">Only members and roles you let in can {st.kind === 'voice' ? 'join its calls and read its chat' : 'see and read it'}.</span>
                </span>
                <input type="checkbox" checked={st.isPrivate} onchange={(e) => patchCreateChannel({ isPrivate: e.currentTarget.checked })}>
                <span class="neon-toggle"></span>
            </label>
        {/if}

        {#if st.error}<p class="ccm-error">{st.error}</p>{/if}

        <div class="ccm-actions">
            <button class="cs-btn-text" onclick={() => h.close()}>Cancel</button>
            <button class="cs-btn-save" disabled={!valid || st.busy} onclick={submit}>{st.busy ? 'Creating…' : `Create ${noun}`}</button>
        </div>
    </div>
</div>
