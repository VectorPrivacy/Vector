<script>
    // What Tor or I2P does for you: the body of the network explainer, shared by the login
    // screen and Settings. `router` slots the login's pre-login I2P router setup under the
    // points; `inSettings` drops the line pointing at Settings.
    import NetGlyph from '../ui/NetGlyph.svelte';

    let { kind, openLink, inSettings = false, router = null } = $props();
</script>

{#snippet learn(label, key)}
    <button type="button" class="lg-net-learn" onclick={() => openLink(key)}>
        {label}
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M14 4h6v6M20 4l-9 9M18 14v4a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4"/>
        </svg>
    </button>
{/snippet}

{#if kind === 'i2p'}
    <div class="lg-net-glyph tor-state-connected" aria-hidden="true"><NetGlyph kind="i2p" /></div>
    <h3 id="lg-net-title">I2P Network</h3>
    <p class="lg-net-lead">Route Vector through your own I2P router so relays and servers never see your IP address.</p>
    <ul class="lg-net-points">
        <li><b>IP Obfuscation</b>: relays see I2P, not you</li>
        <li><b>No Exit Needed</b>: .i2p relays never leave the network</li>
        <li><b>No Central Directory</b>: nothing central to block or seize</li>
        <li><b>One-Way Tunnels</b>: sending and receiving take separate paths</li>
    </ul>
    {@render router?.()}
    <p class="lg-net-note"><b>Expect slower connections.</b> Starting can take a few minutes. Servers outside I2P are reached through an outproxy, which can see and link the ones you use.</p>
    {#if !inSettings}<p class="lg-net-path">You can change this any time in <span>Settings &gt; Privacy &gt; Routing</span>.</p>{/if}
    <p class="lg-net-disclaimer">I2P is independent software run by its volunteers, not operated by Vector.</p>
    {@render learn('Learn more about I2P', 'i2p')}
{:else}
    <img class="lg-net-logo" src="./icons/tor-logo.svg" alt="Tor" width="119" height="72">
    <h3 id="lg-net-title">Tor Network</h3>
    <p class="lg-net-lead">Route Vector’s connection through Tor so relays and servers never see your real IP address.</p>
    <ul class="lg-net-points">
        <li><b>IP Obfuscation</b>: relays see Tor, not you</li>
        <li><b>Location Privacy</b>: your country stays hidden</li>
        <li><b>ISP Shielding</b>: your provider can’t see which relays you use</li>
        <li><b>Censorship Resistance</b>: reach relays blocked on your network</li>
    </ul>
    <p class="lg-net-note"><b>Expect slower connections.</b> Your traffic takes a longer path through volunteer relays worldwide so messages and media take more time to send and load, noticeably slower than a VPN.</p>
    {#if !inSettings}<p class="lg-net-path">You can change this any time in <span>Settings &gt; Privacy &gt; Routing</span>.</p>{/if}
    <p class="lg-net-disclaimer">Tor is independent software maintained by the Tor Project, not operated by Vector.</p>
    {@render learn('Learn more about Tor', 'torAttribution')}
{/if}
