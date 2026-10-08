<script>
    // A network's connection, step by step, exactly as its status reports it (I2P: router,
    // tunnels, clearnet). Any network that reports steps renders here; `children` adds rows after.
    let { steps = [], kind = '', children } = $props();
</script>

{#if steps.length}
    <div class="net-steps-wrap">
        <header class="tor-circuits-head"><span class="tor-circuits-head-label">Connection</span></header>
        <ol class="net-steps net-steps-{kind}">
            {#each steps as s (s.id)}
                <li class="net-step" data-state={s.state}>
                    <span class="net-step-mark"><span class="net-step-dot"></span></span>
                    <span class="net-step-label">{s.label}</span>
                    <span class="net-step-text">{s.text}</span>
                </li>
            {/each}
            {@render children?.()}
        </ol>
    </div>
{/if}
