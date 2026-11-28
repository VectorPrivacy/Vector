// Opens OPEN with a picked wallpaper staged: the edit bar over the header, the sliders above the composer.
(async () => {
    const rows = [...document.querySelectorAll('.chatlist-contact')];
    rows.find((r) => r.textContent.trim().toLowerCase().startsWith(window.__OPEN_NAME__))?.click();
    await new Promise((r) => setTimeout(r, 800));
    await applyWallpaperPreview(strOpenChat, { path: 'media/portrait.png', was_animated: false, recommended_dim: 60 });
})();
