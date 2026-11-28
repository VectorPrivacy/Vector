// The unlock screen's account pill, then (PICK=1) its account list.
(() => {
    if (window.__PICKER_OPEN__) setTimeout(() => document.getElementById('login-account-picker')?.click(), 600);
})();
