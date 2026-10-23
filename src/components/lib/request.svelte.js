// The open chat's DM request, if it is one: its bar offers Block and Accept, and until
// then what its sender sent stays unloaded. js/render/chatlist/row.js answers the buttons.

const req = $state({ chatId: null });
let handlers = $state.raw({});   // { accept(chatId), block(chatId) }

export function requestState() { return req; }
export function setChatRequest(chatId) { req.chatId = chatId || null; }
export function requestHandlers() { return handlers; }
export function setRequestHandlers(h) { handlers = h || {}; }
