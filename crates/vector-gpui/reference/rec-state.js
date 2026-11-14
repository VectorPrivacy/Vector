// Opens OPEN and holds the voice recorder in REC (TYPE: recording | locked | preview).
(() => {
    openChat(window.__OPEN_NAME__);
    const s = window.__TYPE__ || 'recording';
    VectorSvelte.setVoiceState(s);
    VectorSvelte.setVoiceTimer('0:04');
    if (s === 'preview') VectorSvelte.setVoicePreview({ progress: 2600 / 7400 * 100, time: '0:02' });
})();
