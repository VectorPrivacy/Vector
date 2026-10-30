<script>
    // The slim layout's header: your avatar (the account switcher), search, the QR
    // scanner and the New menu. Search filters every chat as you type, from the list itself.
    import Avatar from '../ui/Avatar.svelte';
    import { accountState } from '../lib/account.svelte.js';
    import { shellHandlers } from '../lib/shell.svelte.js';
    import { consoleState, setConsoleQuery } from '../lib/console.svelte.js';
    const acct = accountState();
    const cc = consoleState();
    const h = () => shellHandlers();
    let accountEl = $state(null);
    let inputEl = $state(null);

    function clear() {
        setConsoleQuery('');
        inputEl?.blur();
    }
</script>

<div id="cc-header" class="cc-header">
    <button class="cc-account" bind:this={accountEl} aria-label="Switch profile" onclick={() => h().consoleSwitchAccount?.(accountEl)}>
        <Avatar src={acct.avatarSrc} size={32} />
        <svg class="cc-account-caret" viewBox="0 0 10 6" aria-hidden="true"><path d="M0.5 0.5h9L5 5.5z" fill="currentColor"/></svg>
    </button>
    <label class="cc-search" class:has-text={!!cc.query}>
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><circle cx="11" cy="11" r="7"/><path d="m20 20-3.6-3.6"/></svg>
        <input bind:this={inputEl} type="text" enterkeyhint="search" autocomplete="off" spellcheck="false" placeholder="Search..."
               value={cc.query} onfocus={() => h().consoleSearch?.()} oninput={(e) => setConsoleQuery(e.currentTarget.value)}
               onkeydown={(e) => { if (e.key === 'Escape') clear(); }}>
        {#if cc.query}
            <button class="cc-search-clear" aria-label="Clear search" onclick={(e) => { e.preventDefault(); clear(); }}>
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" aria-hidden="true"><path d="M7 7l10 10M17 7 7 17"/></svg>
            </button>
        {/if}
    </label>
    <button class="cc-header-btn" aria-label="Scan a QR code" onclick={() => h().consoleScan?.()}>
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3 8V6.5A3.5 3.5 0 0 1 6.5 3H8M16 3h1.5A3.5 3.5 0 0 1 21 6.5V8M21 16v1.5a3.5 3.5 0 0 1-3.5 3.5H16M8 21H6.5A3.5 3.5 0 0 1 3 17.5V16M3 12h18"/></svg>
    </button>
    <button class="cc-header-btn" aria-label="New" onclick={(e) => h().consoleNewMenu?.(e.currentTarget)}>
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M12 4v16M4 12h16"/></svg>
    </button>
</div>
