// Opens OPEN and transcribes its first voice message.
(async () => {
    openChat(window.__OPEN_NAME__);
    const sel = '.audio-transcribe-btn';
    for (let i = 0; i < 60 && !document.querySelector(sel); i++) await new Promise((r) => setTimeout(r, 50));
    document.querySelector(sel)?.click();
})();
setTimeout(() => {
    const m = document.querySelector('.audio-transcribe-btn')?.closest('.msg, [id]');
    m?.scrollIntoView({ block: 'start' });
}, 1200);
