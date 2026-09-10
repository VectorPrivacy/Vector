<script>
    // The Invites screen: the account's invite code, a copy button and a share link. The
    // code is fetched by the opener; this shows whichever phase that fetch is in.
    import { invites } from '../lib/invitescreen.svelte.js';
    const st = invites;
    let copied = $state(false);
    async function copy() {
        if (st.phase !== 'ok') return;
        await navigator.clipboard.writeText(st.code);
        copied = true;
        setTimeout(() => { copied = false; }, 2000);
    }
    const label = $derived(st.phase === 'ok' ? st.code : st.phase === 'error' ? 'Error loading code' : 'Loading');
</script>

<div class="invites-container">
    <div class="invites-header">
        <span class="icon icon-gift invites-main-icon"></span>
        <h1 class="invites-title">Invite Code</h1>
    </div>

    <div class="invites-content">
        <p class="invites-description">
            Invite friends to Vector and unlock exclusive badges, earn additional rewards, and help grow the community!
        </p>

        <div class="invite-code-section">
            <h3 class="invite-code-label">Your Invite Code</h3>
            <div class="invite-code-container">
                <span id="invite-code" class="invite-code">{label}</span>
                <button class="btn invite-code-copy-btn" aria-label="Copy invite code" onclick={copy}>
                    <span class="icon" class:icon-copy={!copied} class:icon-check={copied}></span>
                </button>
            </div>

            <p class="invite-code-hint">Share this invite code with friends.</p>
        </div>
        <a id="invite-code-twitter" class="invite-code-twitter-link" title="Post on Twitter (X)" href={st.xUrl || null}>
            <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor">
                <path d="M18.244 2.25h3.308l-7.227 8.26 8.502 11.24H16.17l-5.214-6.817L4.99 21.75H1.68l7.73-8.835L1.254 2.25H8.08l4.713 6.231zm-1.161 17.52h1.833L7.084 4.126H5.117z"/>
            </svg>
            Share on Twitter (X)
        </a>
    </div>
</div>
