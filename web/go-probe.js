// Framed by vectorapp.io's /go page: tells it whether this browser uses Vector Web, so it can
// lead with the right button. One yes or no, posted only to that page, never over the network.
let used = false;
try { used = localStorage.getItem('vector-web') === '1'; } catch { /* storage refused: say no */ }
for (const origin of ['https://vectorapp.io', 'https://www.vectorapp.io']) {
    parent.postMessage({ vectorWeb: used }, origin);
}
