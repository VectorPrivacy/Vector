// Opens mo's chat, where the fixtures put two PIVX payments.
(() => {
    const rows = [...document.querySelectorAll('.chatlist-contact')];
    rows.find((r) => r.textContent.trim().toLowerCase().startsWith('mo'))?.click();
})();
