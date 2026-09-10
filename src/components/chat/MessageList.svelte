<script>
    // The chat view's message list. Owns #chat-messages' children: one keyed
    // each over the rendered window, with day separators, the unread divider, system
    // events and rows all DERIVED from the slice (deriveWindow). The engine sets the
    // window and flushes; nothing inserts or removes rows by hand any more.
    //
    // The DOM stays flat and identical to the vanilla list: rows and separators are
    // direct children (the engine still walks `children` by id), overlays the vanilla
    // side appends after the each (toolbar, empty state, notices) are untouched.
    import { windowState, dividerState, noticeState } from '../lib/chatview.svelte.js';
    import { deriveWindow } from '../lib/chatwindow.js';
    import MessageRow from './MessageRow.svelte';
    import MessageToolbar from './MessageToolbar.svelte';
    import { toolbarHost, toolbarSwipe, toolbarEls, toolbarHandlers } from '../lib/toolbar.svelte.js';
    import SystemEvent from './SystemEvent.svelte';

    let { h } = $props();   // h: ListHelpers (js/render/chat/message-row.js)

    const win = windowState();
    const divider = dividerState();
    const notices = noticeState();
    const host = toolbarHost();
    const swipe = toolbarSwipe();
    const tbEls = toolbarEls();
    const tbh = () => toolbarHandlers();
    function bindHost(node) { tbEls.host = node; return { destroy() { tbEls.host = null; } }; }

    const items = $derived.by(() => {
        win.seq;
        if (!win.chatId || !win.topId || !win.bottomId) return [];
        const msgs = h.messages(win.chatId) || [];
        if (!msgs.length) return [];
        let start = -1, end = -1;
        for (let i = 0; i < msgs.length; i++) {
            if (start === -1 && msgs[i].id === win.topId) start = i;
            if (msgs[i].id === win.bottomId) { end = i + 1; if (start !== -1) break; }
        }
        // An anchor that left the array (a removed message) must not empty the view:
        // fall back to the nearest edge; the engine re-seats real anchors on its next op.
        if (end === -1) end = msgs.length;
        if (start === -1) start = Math.max(0, end - h.maxRows);
        if (end <= start) return [];
        return deriveWindow(msgs, start, end, h.rules);
    });
    // A row's key outlives its id: a send swaps the pending id for the real one, and the
    // same row must carry on (the media inside it keeps playing) rather than remount.
    function rowKey(m) { return m._key || (m._key = m.id); }

    // Only the newest own message shows its Sent status.
    const lastMineId = $derived.by(() => { for (let i = items.length - 1; i >= 0; i--) { const m = items[i].msg; if (m.mine && !m.pending && !m.failed) return m.id; } return null; });
    const dividerId = $derived(divider.targetId);
    const dividerAfter = $derived(divider.after);
</script>

<!-- Before the rows, never after: the list's bottom clearance rides its last child, which
     must stay the newest row. Both are absolutely positioned, so their order is invisible. -->
<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div id="dmsg-toolbar" hidden={!host.open} data-target={host.target || undefined} style:top={host.top || null} style:left={host.left || null}
     use:bindHost onmouseenter={() => tbh().hoverIn?.()} onmouseleave={() => tbh().hoverOut?.()} onclick={(e) => tbh().click?.(e)}>
    <MessageToolbar />
</div>
<div class="dmsg-swipe-reply" hidden={!swipe.visible} class:past={swipe.past}
     style:top={swipe.top || null} style:left={swipe.left || null} style:opacity={swipe.opacity} style:transform={swipe.transform} style:transition={swipe.transition || null}>
    <span class="dmsg-swipe-reply__icon icon icon-reply"></span>
</div>

{#each items as it (rowKey(it.msg))}
    {#if it.dayBreak}
        <p class="msg-inline-timestamp date-divider">{@html h.dayLabel(it.msg.at)}</p>
    {/if}
    {#if dividerId === it.msg.id && !dividerAfter}
        <p class="msg-inline-timestamp unread-divider">New</p>
    {/if}
    {#if it.kind === 'system'}
        <SystemEvent msg={it.msg} merged={it.merged} mergeCount={it.mergeCount} h={h.row} />
    {:else}
        <MessageRow msg={it.msg} sender={h.senderFor(it.msg)} streak={it.streak} ctx={h.ctxFor(it.msg)} lastMine={it.msg.id === lastMineId} h={h.row} />
    {/if}
    {#if dividerId === it.msg.id && dividerAfter}
        <p class="msg-inline-timestamp unread-divider">New</p>
    {/if}
{/each}

{#if notices.empty}
    <div class="chat-empty-state">
        <!-- .icon is position:absolute: it needs a relative, sized wrapper. -->
        <div class="chat-empty-state-icon"><span class="icon icon-users-multi"></span></div>
        <h3>Welcome to {notices.empty}</h3>
        <p>This is the very beginning of the channel, say hello! 👋</p>
    </div>
{/if}
{#if notices.blocked}
    <p class="msg-inline-timestamp" style="margin-bottom: 20px;">Blocked: you won't receive new messages from them</p>
{/if}
{#if notices.dissolved}
    <p class="msg-inline-timestamp" style="margin-bottom: 20px;">{notices.dissolved} was dissolved by the owner.</p>
{/if}
{#if notices.migrated}
    <p class="msg-inline-timestamp" style="margin-bottom: 20px;">This community has upgraded to Concord v2.</p>
{/if}

<!-- No <style>: the global rules cascade; the DOM is the vanilla list's, flat. -->
