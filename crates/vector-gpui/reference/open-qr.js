// Opens the named contact's profile (OPEN), then its QR overlay.
(() => {
    const want = window.__OPEN_NAME__;
    const p = arrProfiles.find((p) => (p.nickname || p.name || '').toLowerCase().startsWith(want));
    openProfile(p);
    setTimeout(() => document.getElementById('profile-qr-btn')?.click(), 300);
})();
