// The chat list's New Message panel; PICK=<n> selects the nth contact.
(() => {
    setTimeout(() => openNewChat(), 200);
    if (window.__PICK__) setTimeout(() => document.querySelectorAll('#create-group-list .member-row, #create-group-list [class*=member]')[window.__PICK__ - 1]?.click(), 900);
})();
