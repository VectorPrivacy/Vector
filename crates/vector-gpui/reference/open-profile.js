// Opens a profile: the named contact's (OPEN, via its chat), else our own.
(() => {
    const want = window.__OPEN_NAME__;
    if (!want) return openProfile();
    const p = arrProfiles.find((p) => (p.nickname || p.name || '').toLowerCase().startsWith(want));
    previousChatBeforeProfile = p?.id || '';
    openProfile(p);
})();
