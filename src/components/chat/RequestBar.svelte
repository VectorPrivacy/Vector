<script>
    // Under the header of a DM from someone new: Block, or Accept to let what they
    // sent load as your settings allow.
    import { requestState, requestHandlers } from '../lib/request.svelte.js';
    const req = requestState();
    const h = () => requestHandlers();
</script>

{#if req.chatId}
    <div class="request-bar" role="region" aria-label="Message request">
        <svg class="request-bar-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M12 3 4.5 6v5.5c0 4.6 3.2 8.4 7.5 9.5 4.3-1.1 7.5-4.9 7.5-9.5V6L12 3Z"/><path d="M12 8.5v4M12 15.5h.01"/>
        </svg>
        <div class="request-bar-text">
            <span class="request-bar-title">Message request</span>
            <span class="request-bar-sub">{req.pictures ? 'Accept to load media & profile' : 'Accept to load media'}</span>
        </div>
        <button class="request-bar-btn is-block" onclick={() => h().block?.(req.chatId)}>Block</button>
        <button class="request-bar-btn is-accept" onclick={() => h().accept?.(req.chatId)}>Accept</button>
    </div>
{/if}
