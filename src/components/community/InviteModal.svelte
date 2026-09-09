<script>
    // Invite to a community: the link section and the direct-invite contact picker, with
    // the footer CTA that morphs from Done to Invite N as contacts are picked.
    import { inviteModalState, inviteModalHandlers, setInviteModalPicker } from '../lib/invitemodal.svelte.js';
    import InviteLinks from './InviteLinks.svelte';
    import ContactPicker from '../people/ContactPicker.svelte';
    const st = inviteModalState();
    const h = () => inviteModalHandlers();
    let picker = $state(null);
    $effect(() => { setInviteModalPicker(picker); return () => setInviteModalPicker(null); });
</script>

{#if st.open}
    <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
    <div class="modal-overlay" onclick={(e) => { if (e.target === e.currentTarget && !st.busy) h().close?.(); }}>
        <div class="modal-box cmt-modal" class:cmt-busy={st.busy}>
            <div class="cmt-header">
                <div class="cmt-header-icon"><span class="icon icon-users-multi"></span></div>
                <div class="cmt-header-text">
                    <h3 class="cmt-title">Invite to {st.name}</h3>
                    <p class="cmt-subtitle">Bring people into your community.</p>
                </div>
                <button class="relay-dialog-close cmt-close-x" disabled={st.busy} onclick={() => h().close?.()}>&times;</button>
            </div>

            <div class="cmt-body">
                <div><InviteLinks h={h().links} /></div>

                <section class="cmt-section">
                    <div class="cmt-section-head">
                        <span class="icon icon-add-user"></span>
                        <div>
                            <p class="cmt-section-title">Direct Invites</p>
                            <p class="cmt-section-desc">Pick contacts to invite, or paste an npub to add someone new.</p>
                        </div>
                    </div>
                    <div class="emoji-search-container" style="padding: 0; background: transparent; margin-bottom: 8px; isolation: isolate;">
                        <span class="emoji-search-icon icon icon-search"></span>
                        <input id="cmt-npub" type="text" placeholder="Search" autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"
                               disabled={st.busy} bind:value={st.search} oninput={() => h().searchInput?.(st.search)}
                               style="flex: 1; box-sizing: border-box; margin: 0; padding: 10px 12px 10px 44px; background-color: transparent; border: 1px solid rgba(57, 57, 57, 0.5); border-radius: 8px; color: #fff; font-size: 16px;" />
                    </div>
                    <div id="cmt-contacts" class="cmt-contacts"><ContactPicker bind:this={picker} {...h().contactProps} /></div>
                </section>

                <!-- Rides inside the single scroll region so the primary action is always reachable. -->
                <div class="cmt-footer">
                    <div class="cmt-status" class:cmt-err={st.status.error} class:cmt-ok={!!st.status.text && !st.status.error}>{#if st.status.text}{st.status.text}{/if}</div>
                    <button class="cmt-btn cmt-btn-ghost cmt-cta cmt-close" class:has-selection={st.selected > 0} disabled={st.busy || st.ctaBusy} onclick={() => h().cta?.()}>
                        <span class="cmt-cta-face cmt-cta-face-done">Done</span>
                        <span class="cmt-cta-face cmt-cta-face-invite"><span class="icon icon-send"></span>Invite <span>{st.selected || ''}</span></span>
                    </button>
                </div>
            </div>
        </div>
    </div>
{/if}
