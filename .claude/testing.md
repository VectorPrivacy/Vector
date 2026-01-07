# Vector MCP Testing Guide

Notes for using the Tauri MCP server to interact with Vector.

## Connecting

```
tauri_driver_session action: "start"
```

Check status with `action: "status"`.

## Key Selectors

### Navigation
- **Back button**: `#chat-back-btn`
- **Profile tab**: `.nav-btn` containing "Profile"
- **Chat tab**: `.nav-btn` containing "Chat"
- **Settings tab**: `.nav-btn` containing "Settings"

### Chat List (main screen)
- **Chat names**: `h4.cutoff` (but matches ALL chats - use JS or coordinates instead)
- **New Chat button**: Look for "New Chat" text
- **Group Chat button**: Look for "Group Chat" text

### Chat View
- **Message input**: `#chat-input`
- **Send**: Just press Enter after typing

## Reliable Patterns

### Clicking on a specific chat by name
CSS selectors with `:contains()` don't work. Use JavaScript:

```javascript
(() => {
  const all = Array.from(document.querySelectorAll('h4'));
  for (const el of all) {
    if (el.textContent === 'CONTACT_NAME') {
      const rect = el.getBoundingClientRect();
      return JSON.stringify({x: rect.x + rect.width/2, y: rect.y + rect.height/2});
    }
  }
  return 'not found';
})()
```

Then click at those coordinates.

### Sending a message
1. Type into `#chat-input`:
   ```
   tauri_webview_keyboard action: "type", selector: "#chat-input", text: "your message"
   ```
2. Press Enter:
   ```
   tauri_webview_keyboard action: "press", key: "Enter"
   ```

### Finding elements at a point
Useful for reverse-engineering the DOM:
```javascript
(() => {
  const el = document.elementFromPoint(x, y);
  return el ? el.outerHTML.substring(0, 500) : 'nothing';
})()
```

## Gotchas

1. **Generic selectors match first element**: `h4.cutoff` will click the FIRST chat, not a specific one. Always use coordinates or JS for specific items.

2. **`:contains()` is not valid CSS**: Use JavaScript to find elements by text content.

3. **`:has()` with `:contains()` fails**: These jQuery-style selectors don't work in native CSS.

4. **Coordinates are reliable**: When in doubt, get bounding rect via JS and click coordinates directly.

## Common Flows

### Send message to contact
1. Screenshot to see current state
2. If not on chat list, click `#chat-back-btn`
3. Find contact's h4 element coordinates via JS
4. Click at coordinates
5. Type into `#chat-input`
6. Press Enter
7. Screenshot to confirm

### Navigate to Settings
1. Click at bottom nav area where Settings icon is (~685, 1020 based on 3-tab layout)
2. Or find via JS: `document.querySelector('[data-tab="settings"]')` or similar
