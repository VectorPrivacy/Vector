// Open the fixture community from the rail; window.__CH__ names a channel to open after.
(() => {
    setTimeout(() => [...document.querySelectorAll('*')].find((e) => e.children.length === 0 && e.textContent.trim() === 'Vector Devs')?.closest('[class*="rail"], div')?.click(), 400);
    if (window.__CH__) setTimeout(() => [...document.querySelectorAll('*')].find((e) => e.children.length === 0 && e.textContent.trim() === window.__CH__)?.click(), 1100);
})();
