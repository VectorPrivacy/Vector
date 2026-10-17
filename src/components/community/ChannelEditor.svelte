<script>
    // A channel's name, topic and view. The view only tells a client how to open it.
    import { channelEditorState, closeChannelEditor } from '../lib/channel-editor.svelte.js';

    // h: save({ communityId, channelId, name, topic, view })
    let { h } = $props();
    const st = channelEditorState();
    const VIEWS = [
        { id: 'chat', name: 'Text', desc: 'Messages first.' },
        { id: 'forum', name: 'Forum', desc: 'Opens on its threads.' },
        { id: 'voice', name: 'Voice', desc: 'Opens its call, with the chat beside it.' },
    ];
    const enc = new TextEncoder();
    const nameBytes = $derived(enc.encode(st.name).length);
    const topicBytes = $derived(enc.encode(st.topic).length);
    const valid = $derived(!!st.name.trim() && nameBytes <= 64 && topicBytes <= 1024);
    function save() {
        if (!valid || st.busy) return;
        h.save({ communityId: st.communityId, channelId: st.channelId, name: st.name.trim(), topic: st.topic.trim() || null, view: st.view });
    }
</script>

{#if st.open}
    <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
    <div class="modal-overlay" onclick={(e) => { if (e.target === e.currentTarget && !st.busy) closeChannelEditor(); }}>
        <div class="modal-box cmt-modal ce-modal" class:cmt-busy={st.busy}>
            <div class="cmt-header">
                <div class="cmt-header-icon"><span class="icon icon-channel-hash"></span></div>
                <div class="cmt-header-text">
                    <h3 class="cmt-title">Edit Channel</h3>
                    <p class="cmt-subtitle">How it shows and how it opens.</p>
                </div>
                <button class="relay-dialog-close cmt-close-x" disabled={st.busy} onclick={closeChannelEditor}>&times;</button>
            </div>
            <div class="cmt-body">
                <div class="cs-field">
                    <div class="cs-field-head">
                        <label class="cs-label" for="ce-name">Channel Name</label>
                        <span class="cs-count" class:warn={nameBytes > 64 || !st.name.trim()}>{nameBytes}/64</span>
                    </div>
                    <input id="ce-name" class="cs-input" type="text" disabled={st.busy} bind:value={st.name}
                           onkeydown={(e) => { if (e.key === 'Enter') save(); }}>
                </div>
                <div class="cs-field cs-field-gap">
                    <div class="cs-field-head">
                        <label class="cs-label" for="ce-topic">Topic</label>
                        <span class="cs-count" class:warn={topicBytes > 1024}>{topicBytes}/1024</span>
                    </div>
                    <textarea id="ce-topic" class="cs-input cs-textarea" rows="3" disabled={st.busy} bind:value={st.topic}
                              placeholder="What this channel is for"></textarea>
                </div>
                <div class="cs-field cs-field-gap">
                    <span class="cs-label">Opens As</span>
                    <div class="ce-views">
                        {#each VIEWS as v (v.id)}
                            <button class="ce-view" class:on={st.view === v.id} disabled={st.busy} onclick={() => { st.view = v.id; }}>
                                <span class="cs-perm-name">{v.name}</span>
                                <span class="cs-perm-desc">{v.desc}</span>
                            </button>
                        {/each}
                    </div>
                </div>
                <div class="cmt-footer">
                    <div class="cmt-status" class:cmt-err={!!st.error}>{st.error}</div>
                    <button class="cs-btn-text" disabled={st.busy} onclick={closeChannelEditor}>Cancel</button>
                    <button class="cs-btn-save" disabled={!valid || st.busy} onclick={save}>{st.busy ? 'Saving...' : 'Save'}</button>
                </div>
            </div>
        </div>
    </div>
{/if}
