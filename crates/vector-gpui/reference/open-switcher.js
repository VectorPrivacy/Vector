// Opens our own profile, then its My Profile account switcher (EDIT=1: in delete mode).
(() => {
    openProfile();
    setTimeout(() => document.getElementById('my-profile-switcher')?.click(), 300);
    if (window.__SWITCHER_EDIT__) setTimeout(() => document.getElementById('profile-switcher-trash-toggle')?.click(), 700);
})();
