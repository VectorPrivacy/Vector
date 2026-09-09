// The PIVX wallet dialogs. Each is a fade dialog whose form lives in the store so pivx.js
// reads what the user typed from here rather than from the DOM.
import { fadeDialog } from './dialogs.svelte.js';

export const pivxDeposit = fadeDialog({ address: '', received: 0 });   // received > 0 swaps the spinner for the tick

export const pivxSend = fadeDialog({
    recipient: '', mode: 'quick', loading: true, promos: [], error: '',
    selectedCode: '', amount: '', available: 0, busy: false,
});

export const pivxWithdraw = fadeDialog({ address: '', amount: '', available: 0, busy: false });

export const pivxSettings = fadeDialog({ address: '', currencies: [], currency: '', currenciesLoading: true });
