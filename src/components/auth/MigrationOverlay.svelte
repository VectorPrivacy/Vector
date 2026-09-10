<script>
    // The non-dismissable progress card shown while messages are re-encrypted.
    import { migrationState } from '../lib/credential.svelte.js';
    const m = migrationState();
</script>

<div id="encryption-migration-overlay" class="encryption-migration-overlay" class:active={m.open}>
    <div class="encryption-migration-modal">
        <div class="encryption-migration-icon">
            <span class="icon icon-locked"></span>
        </div>
        <h2>{m.title}</h2>
        <p id="encryption-migration-phase" class="encryption-migration-phase">{m.phase}</p>
        <div class="progress-bar-track">
            <div class="progress-bar-fill" style:width="{m.pct}%">
                <div class="progress-bar-glow"></div>
            </div>
            <span class="progress-text">{m.pct}%</span>
        </div>
        <p class="encryption-migration-warning">
            <span class="encryption-warning-icon"><span class="icon icon-info"></span></span>
            Do not close the app during this process
        </p>
    </div>
</div>
