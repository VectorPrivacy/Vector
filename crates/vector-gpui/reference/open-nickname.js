// Opens the named contact's profile (OPEN), then its More menu's Nickname popup.
(() => {
    const want = window.__OPEN_NAME__;
    const p = arrProfiles.find((p) => (p.nickname || p.name || '').toLowerCase().startsWith(want));
    openProfile(p);
    setTimeout(() => document.getElementById('profile-option-more')?.click(), 300);
    setTimeout(() => { if (!window.__MORE_ONLY__) document.querySelector('.profile-more-item')?.click(); }, 600);
})();
