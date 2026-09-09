// QR scanning (mobile). Camera frames come from getUserMedia — the Android
// WebChromeClient grants VIDEO_CAPTURE against the app's CAMERA permission —
// and decoding is the vendored jsQR. No native scanner, no Play Services.

let qrScanStream = null;
let qrScanRafId = 0;
let qrScanToastGate = false;
// The scanner's video element, handed over by the component once it mounts.
let qrScanVideo = null;
VectorSvelte.mountQrScanner({ h: { video: (el) => { qrScanVideo = el; }, close: () => closeQrScanner() } });

/**
 * Pick the primary rear camera. Android exposes every lens (macro, ultra-wide,
 * tele, depth) as its own device and facingMode alone can land on the macro.
 * The main camera is HAL id 0 by convention, so prefer the back-facing device
 * with the lowest numeric id in its label, penalising special-lens labels.
 * Labels are only readable once a camera permission has been granted.
 */
async function pickPrimaryRearCamera() {
    const devices = await navigator.mediaDevices.enumerateDevices();
    const backs = devices.filter(d => d.kind === 'videoinput' && /back|rear|environment/i.test(d.label));
    if (!backs.length) return null;
    const score = (label) => {
        const idx = parseInt(label.match(/\d+/)?.[0] ?? '99', 10);
        return idx + (/macro|depth|ultra|tele|wide/i.test(label) ? 100 : 0);
    };
    backs.sort((a, b) => score(a.label) - score(b.label));
    return backs[0].deviceId;
}

const QR_SCAN_RESOLUTION = { width: { ideal: 1280 }, height: { ideal: 720 } };

async function openQrScanner() {
    if (qrScanStream) return;
    const video = qrScanVideo;
    try {
        qrScanStream = await navigator.mediaDevices.getUserMedia({
            video: { facingMode: 'environment', ...QR_SCAN_RESOLUTION },
            audio: false
        });
    } catch (e) {
        showToast(e?.name === 'NotAllowedError' ? 'Camera permission denied' : 'Camera unavailable');
        return;
    }
    // Swap to the primary rear lens if facingMode landed elsewhere. Stop the
    // first stream before opening the next — phones commonly refuse to hold
    // two physical cameras open at once.
    try {
        const primary = await pickPrimaryRearCamera();
        const current = qrScanStream.getVideoTracks()[0]?.getSettings().deviceId;
        if (primary && current && primary !== current) {
            for (const track of qrScanStream.getTracks()) track.stop();
            qrScanStream = null;
            qrScanStream = await navigator.mediaDevices.getUserMedia({
                video: { deviceId: { exact: primary }, ...QR_SCAN_RESOLUTION },
                audio: false
            }).catch(() => navigator.mediaDevices.getUserMedia({
                video: { facingMode: 'environment', ...QR_SCAN_RESOLUTION },
                audio: false
            }));
        }
    } catch (_) { /* keep whatever camera we have */ }
    if (!qrScanStream) {
        showToast('Camera unavailable');
        return;
    }
    // Continuous autofocus where the lens supports it — QR blur is the enemy
    const track = qrScanStream.getVideoTracks()[0];
    if (track?.getCapabilities?.()?.focusMode?.includes('continuous')) {
        track.applyConstraints({ advanced: [{ focusMode: 'continuous' }] }).catch(() => {});
    }
    video.srcObject = qrScanStream;
    // Reveal only once a frame has actually been presented
    const reveal = () => VectorSvelte.setQrScanner({ live: true });
    if (video.requestVideoFrameCallback) video.requestVideoFrameCallback(reveal);
    else video.addEventListener('loadeddata', reveal, { once: true });
    video.play().catch(() => {});
    VectorSvelte.setQrScanner({ active: true });
    pushBack('qr-scanner', closeQrScanner);

    const canvas = document.createElement('canvas');
    const ctx = canvas.getContext('2d', { willReadFrequently: true });
    let lastDecode = 0;
    const tick = (now) => {
        qrScanRafId = requestAnimationFrame(tick);
        if (now - lastDecode < 100 || video.readyState < 2) return;
        lastDecode = now;
        // Decode a centred square crop — matches the viewfinder and caps jsQR's cost
        const side = Math.min(video.videoWidth, video.videoHeight);
        if (!side) return;
        const sx = (video.videoWidth - side) / 2, sy = (video.videoHeight - side) / 2;
        const target = Math.min(side, 640);
        canvas.width = target;
        canvas.height = target;
        ctx.drawImage(video, sx, sy, side, side, 0, 0, target, target);
        const img = ctx.getImageData(0, 0, target, target);
        const hit = jsQR(img.data, target, target, { inversionAttempts: 'attemptBoth' });
        if (hit?.data) handleScannedQr(hit.data.trim());
    };
    qrScanRafId = requestAnimationFrame(tick);
}

function closeQrScanner() {
    cancelAnimationFrame(qrScanRafId);
    qrScanRafId = 0;
    if (qrScanStream) {
        for (const track of qrScanStream.getTracks()) track.stop();
        qrScanStream = null;
    }
    if (qrScanVideo) qrScanVideo.srcObject = null;
    VectorSvelte.setQrScanner({ active: false, live: false });
    popBack('qr-scanner');
}

/**
 * The scanner is entered from the New Chat screen; put that screen away
 * before routing or the destination view stacks on top of it. Awaited
 * BEFORE opening the destination — closeChat hides the profile view too.
 */
async function dismissNewChatForScan() {
    if (VectorSvelte.paneShown('chatNew')) await closeChat();
}

/**
 * Extract the actionable payload from free-form contact input — shared by
 * the QR scanner and the New Chat text box. Wrappers are irrelevant (any
 * URL, nostr: prefix, or bare): the npub or Concord invite inside routes.
 */
function parseContactInput(text) {
    const t = (text || '').trim();
    if (!t) return null;
    // Community invites outrank npubs — an invite link can contain both
    if (isCommunityInviteUrl(t)) return { kind: 'invite', url: t };
    // A v2 invite (naddr + key fragment) inside an unrecognised wrapper:
    // extract it and rebuild the bare form the join flow already parses
    const naddr = t.match(/(naddr1[a-z0-9]{20,})[^#\s]*#([A-Za-z0-9_-]{20,})/i);
    if (naddr) return { kind: 'invite', url: `${naddr[1].toLowerCase()}#${naddr[2]}` };
    // An npub anywhere in the payload (bare, nostr:, or any profile URL)
    const npub = t.match(/npub1[qpzry9x8gf2tvdw0s3jn54khce6mua7l]{58}/)?.[0];
    if (npub) return { kind: 'npub', npub };
    return null;
}

/** Route a decoded QR payload: profile npubs and Community invites. */
async function handleScannedQr(text) {
    const parsed = parseContactInput(text);
    if (parsed) {
        closeQrScanner();
        await dismissNewChatForScan();
        if (parsed.kind === 'invite') {
            executeDeepLinkAction({ action_type: 'community_invite', target: parsed.url });
        } else {
            executeDeepLinkAction({ action_type: 'profile', target: parsed.npub });
        }
        return;
    }
    // Unrecognised: stay live so the user can re-aim, without toast-spamming
    if (!qrScanToastGate) {
        qrScanToastGate = true;
        showToast('Not a Vector QR code');
        setTimeout(() => { qrScanToastGate = false; }, 3000);
    }
}

