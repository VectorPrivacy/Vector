// Where the user is: the topmost screen that names a place, derived from the screens' own
// state. Vector Web's address bar and history follow it; the native app has no listener.
// The place's link grammar is vector-core's deep_link.rs.
import { untrack } from 'svelte';
import { paneShown } from './shell.svelte.js';
import { mktState } from './marketplace.svelte.js';
import { packDetails } from './packdetails.svelte.js';
import { settingsScreen } from './settings.svelte.js';
import { profileViewState, openChatId } from './signals.svelte.js';
import { consoleFilter } from './console.svelte.js';

// The message last jumped to, and the chat it's in.
let jumped = $state.raw({ chat: null, id: null });
export function setPlaceMessage(chat, id) { jumped = { chat: chat || null, id: id || null }; }

function placeNow(communityOf) {
    const pack = packDetails();
    if (pack) return { kind: 'pack', naddr: pack.naddr };
    // The Nexus opens over whatever tab was showing; any other tab closes it.
    const mkt = mktState();
    if (mkt.panelOpen && !mkt.panelClosing) return { kind: 'nexus', app: mkt.detailsId || '' };
    if (paneShown('settings')) return { kind: 'settings', ...settingsScreen().at };
    if (paneShown('profile')) return { kind: 'profile', npub: profileViewState().id };
    const chat = openChatId();
    if (chat && paneShown('chat')) {
        return { kind: 'chat', chat, community: communityOf(chat), message: jumped.chat === chat ? jumped.id : null };
    }
    return paneShown('chats') && consoleFilter() === 'requests' ? { kind: 'requests' } : { kind: 'home' };
}

let report = null;

/** Hand `onPlace` the place now and every one after it; `communityOf(chatId)` names a channel's community. */
export function watchPlace(communityOf, onPlace) {
    let last = '';
    let landing = null;
    report = () => onPlace(placeNow(communityOf));
    $effect.root(() => {
        $effect(() => {
            // A navigation passes screens on its way (each showPane flushes): only where it
            // lands, once the task is done, is a place.
            if (landing === null) {
                queueMicrotask(() => {
                    const place = landing;
                    landing = null;
                    const key = JSON.stringify(place);
                    if (key === last) return;
                    last = key;
                    onPlace(place);
                });
            }
            landing = placeNow(communityOf);
        });
    });
}

/** Say the place again, unchanged: an address that drifted from it is put back. */
export function repeatPlace() { report?.(); }
