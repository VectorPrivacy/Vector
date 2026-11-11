// Opens our own profile, its Status dialog, then the dialog's emoji picker.
(() => {
    openProfile();
    setTimeout(() => askForStatus(), 300);
    setTimeout(() => document.getElementById('status-emoji-btn')?.click(), 700);
})();
