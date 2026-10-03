// Device transfer: sign this account in on another device (the sender, from Settings only), or
// this device in from another (the receiver, from the sign in screen). The protocol and the key
// stay in Rust; this drives the card.

/** Which side this device is on in the open card. */
let transferSend = false;
/** Bumped by every user step, so a slow answer to an abandoned one changes nothing. */
let transferSeq = 0;
/** The newest transfer the backend has named; events from ids below the floor are abandoned ones. */
let transferId = 0;
let transferFloor = 0;
const TRANSFER_CODE_RE = /^\s*vector transfer code:/i;

const transferState = () => VectorSvelte.transferModal.state();
const transferPatch = (view) => VectorSvelte.transferModal.patch(view);

/**
 * @typedef {Object} TransferHelpers
 * @property {() => void} close
 * @property {() => void} scan
 * @property {() => void} enterCode
 * @property {() => void} showCode
 * @property {() => void} connect
 * @property {() => void} approve
 * @property {() => void} deny
 * @property {() => void} retry
 * @property {(node: Element, text: string) => void} renderQr
 */
document.addEventListener('DOMContentLoaded', () => {
    /** @type {TransferHelpers} */
    const h = {
        close: () => closeTransfer(),
        scan: () => scanTransferCode(),
        enterCode: () => enterTransferCode(),
        showCode: () => showTransferCode(),
        connect: () => joinTransfer(transferState().entry),
        approve: () => approveTransfer(),
        deny: () => denyTransfer(),
        retry: () => retryTransfer(),
        renderQr: (node, text) => renderQrInto(node, text),
    };
    VectorSvelte.setScreen('transferModal', { h });
    window.__TAURI__.event.listen('transfer_state', (e) => applyTransferState(e.payload));
}, { once: true });

/** Whether the card may close now: never mid-send or mid-sign-in. */
function transferClosable() {
    const st = transferState();
    return !st.busy && st.stage !== 'sending' && st.stage !== 'finishing';
}

/** Android back: closes the card when it may close, otherwise stays put. */
function onTransferBack() {
    if (!transferClosable()) {
        pushBack('transfer', onTransferBack);
        return;
    }
    closeTransfer();
}

/** Leave the current transfer behind: its answers and events no longer count. */
function abandonTransfer() {
    transferSeq++;
    transferFloor = transferId + 1;
    invoke('transfer_cancel').catch(() => {});
}

/** A phone being set up starts on scanning, since the device it moves from shows its code by default. */
const scanFirst = () => !transferSend && platformFeatures.is_mobile;

/** Open the card. `send` only from Settings on the signed-in device: never from a link or a scan. */
function openTransfer(send) {
    transferSend = send;
    VectorSvelte.transferModal.open({
        role: send ? 'sender' : 'receiver', stage: 'connecting', code: '', qr: '', expiresAt: 0,
        entry: '', number: '', sas: '', sender: '', name: '', avatar: '', busy: false, unconfirmed: false,
        finishFailed: false, error: '', canScan: !!platformFeatures.is_mobile,
    });
    pushBack('transfer', onTransferBack);
    if (scanFirst()) transferPatch({ stage: 'pick' });
    else showTransferCode();
}

function closeTransfer() {
    if (VectorSvelte.transferModal.closing()) return;
    abandonTransfer();
    popBack('transfer');
    VectorSvelte.transferModal.close();
}

async function showTransferCode() {
    abandonTransfer();
    const seq = transferSeq;
    transferPatch({ stage: 'connecting', error: '', busy: false, number: '', sas: '', finishFailed: false });
    try {
        const started = await invoke('transfer_start', { send: transferSend });
        if (seq !== transferSeq) return;
        transferId = started.id;
        if (transferState().stage === 'connecting') {
            transferPatch({ stage: 'show', code: started.code, qr: started.qr, expiresAt: Date.now() + 5 * 60 * 1000 });
        }
    } catch (e) {
        if (seq === transferSeq) transferPatch({ stage: 'error', error: String(e) });
    }
}

function enterTransferCode() {
    abandonTransfer();
    transferPatch({ stage: 'enter', entry: '', error: '', busy: false });
}

async function joinTransfer(code) {
    const entry = (code || '').trim();
    if (!entry) return;
    abandonTransfer();
    const seq = transferSeq;
    transferPatch({ stage: 'enter', busy: true, error: '' });
    try {
        const started = await invoke('transfer_start', { send: transferSend, code: entry });
        if (seq !== transferSeq) return;
        transferId = started.id;
        transferPatch(transferState().stage === 'enter' ? { stage: 'connecting', busy: false } : { busy: false });
    } catch (e) {
        if (seq === transferSeq) transferPatch({ stage: 'enter', entry: TRANSFER_CODE_RE.test(entry) ? '' : entry, busy: false, error: String(e) });
    }
}

function scanTransferCode() {
    // From the scan-first screen a cancelled scan returns there; elsewhere it falls back to typing.
    if (transferState().stage === 'pick') abandonTransfer();
    else enterTransferCode();
    scanFor((text) => {
        const st = transferState();
        if (!st.active || st.closing || !TRANSFER_CODE_RE.test(text)) return false;
        joinTransfer(text);
        return true;
    });
}

/** Try Again: a failed sign-in retries itself; anything else starts over the way the card began. */
function retryTransfer() {
    if (transferState().finishFailed) {
        finishReceivedTransfer();
    } else if (scanFirst()) {
        abandonTransfer();
        transferPatch({ stage: 'pick', error: '' });
    } else {
        showTransferCode();
    }
}

/** Sender: the number from the new device, then the user's proof, then the account goes. */
async function approveTransfer() {
    const seq = transferSeq;
    const current = () => seq === transferSeq && transferState().stage === 'approve';
    transferPatch({ busy: true, error: '' });
    try {
        await invoke('transfer_check_number', { number: transferState().number.replace(/\D/g, '') });
    } catch (e) {
        if (!current()) return;
        const left = String(e).match(/^TRANSFER_WRONG_NUMBER:(\d+)/)?.[1];
        transferPatch({
            busy: false, number: '',
            error: left ? `That isn't the number on the new device. ${left} ${left === '1' ? 'try' : 'tries'} left.` : String(e),
        });
        return;
    }
    try {
        try {
            await invoke('transfer_approve');
        } catch (e) {
            if (String(e) !== 'CREDENTIAL_REQUIRED' || !current()) throw e;
            const label = fSecurityType === 'password' ? 'password' : 'PIN';
            const sent = await withCurrentCredential('Approve the Transfer', `Enter your ${label} to send this account to the new device.`,
                async (credential) => { await invoke('transfer_approve', { credential }); return true; });
            if (!sent && current()) transferPatch({ busy: false });
        }
    } catch (e) {
        if (current()) transferPatch({ busy: false, error: String(e).includes('BIOMETRIC_CANCELLED') ? '' : String(e) });
    }
}

function denyTransfer() {
    transferSeq++;
    transferFloor = transferId + 1;
    invoke('transfer_deny').catch(() => {});
    popBack('transfer');
    VectorSvelte.transferModal.close();
}

/** Receiver: the account arrived after the signed-in device approved it; sign in with it. */
async function finishReceivedTransfer() {
    transferPatch({ stage: 'finishing', error: '', finishFailed: false });
    try {
        await finishTransfer();
        popBack('transfer');
        VectorSvelte.transferModal.close();
    } catch (e) {
        // The account stays held, so Try Again signs in with it again, unless it has already gone.
        const gone = /Nothing has arrived/.test(String(e));
        transferPatch({
            stage: 'error', finishFailed: !gone,
            error: gone ? 'The account that arrived has expired. Start again.' : `Couldn't finish signing in: ${String(e)}`,
        });
    }
}

/** Stages each side can be told about; anything else came from the other role's flow. */
const TRANSFER_STAGES = {
    receiver: ['match', 'received', 'error'],
    sender: ['approve', 'sending', 'sent', 'error'],
};

function applyTransferState(p) {
    const st = transferState();
    if (!st.active || st.closing || !p || (p.id ?? 0) < transferFloor) return;
    if (!TRANSFER_STAGES[st.role]?.includes(p.stage) || st.stage === 'finishing') return;
    switch (p.stage) {
        case 'match':
            transferPatch({ stage: 'match', sas: p.sas || '', sender: p.sender || '', name: p.name || '', avatar: p.avatar || '' });
            break;
        case 'approve':
            transferPatch({ stage: 'approve', number: '', error: '', busy: false });
            break;
        case 'sending':
            transferPatch({ stage: 'sending' });
            break;
        case 'sent':
            transferPatch({ stage: 'sent', unconfirmed: !!p.unconfirmed, busy: false });
            break;
        case 'received':
            transferPatch({ sender: p.sender || st.sender, name: p.name || st.name, avatar: p.avatar || st.avatar });
            finishReceivedTransfer();
            break;
        case 'error':
            transferPatch({ stage: 'error', error: p.error || 'The transfer stopped.', busy: false });
            break;
    }
}
