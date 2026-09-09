<script>
    // A full takeover for a build older than the account's database. There is no dismiss
    // path: continuing would write an older schema over a newer one.
    import { downgradeBlock } from '../lib/dialogs.svelte.js';
    let { h } = $props();   // h: getLatest(), quit()
    const st = downgradeBlock;
</script>

{#if st.open}
    <div id="downgrade-block" class="downgrade-block">
        <div class="downgrade-card">
            <img class="downgrade-icon" src="./icons/vector_warning.svg" alt="">
            <h2>Vector can't open this account</h2>
            <p>
                Your data was last used with a newer version of Vector. Opening it with this
                older build would damage your message history.
            </p>
            <div class="downgrade-versions">
                <div class="downgrade-version">
                    <span>This build</span>
                    <strong>{st.current}</strong>
                </div>
                <div class="downgrade-versions-sep"></div>
                <div class="downgrade-version">
                    <span>Your data needs</span>
                    <strong>{st.required}</strong>
                </div>
            </div>
            <p class="downgrade-reassure">
                Nothing has been lost. Your messages are intact and will open again on the newer version.
            </p>
            <button class="btn accept-btn btn-bounce" onclick={() => h.getLatest()}>Get the latest version</button>
            <button class="cancel-btn" onclick={() => h.quit()}>Quit Vector</button>
        </div>
    </div>
{/if}
