// Our own profile holding every badge, then (CARD=<n>) the nth badge's card.
(() => {
    window.__BADGES__ = { vector: true, vector_2026: true, tier: 3, bug_hunter: 2, invites: 3 };
    openProfile();
    if (window.__CARD__) setTimeout(() => document.querySelectorAll('#profile-badges img')[window.__CARD__ - 1]?.click(), 900);
})();
