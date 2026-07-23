// Entry for the Svelte island bundle. esbuild compiles this (+ every .svelte it pulls in)
// into src/components.bundle.js as an IIFE exposing the `VectorSvelte` global, so the vanilla
// one-global-scope frontend can call VectorSvelte.mountX(target, props) directly.
import { mount, unmount } from 'svelte';
import { writable, get } from 'svelte/store';
import ContactList from './ContactList.svelte';

/**
 * Mount the community-invite contact picker into `target`. Creates the three bridge stores,
 * hands them to the component, and returns them so the vanilla dialog can drive filter/strangers
 * and read/clear selection. Call `unmountComponent(instance)` on dialog close.
 */
export function mountContactList(target, props = {}) {
    const filter = writable('');
    const strangers = writable([]);
    const selection = writable(new Set());
    const profiles = writable(props.profiles || []);
    const instance = mount(ContactList, { target, props: { ...props, filter, strangers, selection, profiles } });
    return { instance, filter, strangers, selection, profiles };
}

/** Tear down a mounted island (call on dialog close / element removal). */
export function unmountComponent(instance) {
    return unmount(instance);
}

// Store helpers for the vanilla side (read a store synchronously, create ad-hoc stores).
export { get, writable };
