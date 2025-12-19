# Mini Apps (WebXDC) for Vector

Mini Apps are small, isolated web applications that can be shared and run within Vector chats. They are based on the [WebXDC specification](https://webxdc.org/) originally developed by Delta Chat.

## What is a Mini App?

A Mini App is a `.xdc` file, which is simply a ZIP archive containing:

- `index.html` - The main entry point (required)
- `manifest.toml` - Metadata about the app (optional)
- Any other web assets (JS, CSS, images, etc.)

## Creating a Mini App

### Basic Structure

```
my-app.xdc/
├── index.html      # Required: Main entry point
├── manifest.toml   # Optional: App metadata
├── icon.png        # Optional: App icon
├── style.css       # Optional: Styles
└── app.js          # Optional: JavaScript
```

### manifest.toml

```toml
name = "My Mini App"
description = "A simple example Mini App"
version = "1.0.0"
icon = "icon.png"
```

### index.html Example

```html
<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>My Mini App</title>
    <script src="miniapp.js"></script>
</head>
<body>
    <h1>Hello from Mini App!</h1>
    <div id="status"></div>
    <button onclick="sendHello()">Send Hello</button>
    
    <script>
        // The webxdc API is available globally
        document.getElementById('status').textContent = 
            'Running as: ' + window.webxdc.selfName;
        
        // Listen for updates from other participants
        window.webxdc.setUpdateListener(function(update) {
            console.log('Received update:', update);
        });
        
        function sendHello() {
            window.webxdc.sendUpdate({
                payload: { message: 'Hello!' }
            }, 'Sent a greeting');
        }
    </script>
</body>
</html>
```

## The webxdc API

Mini Apps have access to a `window.webxdc` object with the following API:

### Properties

- `webxdc.selfAddr` - A unique identifier for the current user in this chat
- `webxdc.selfName` - The display name of the current user

### Methods

#### `webxdc.setUpdateListener(listener, serial)`

Set a callback to receive updates from other participants.

```javascript
webxdc.setUpdateListener(function(update) {
    console.log('Received:', update.payload);
}, 0);
```

#### `webxdc.sendUpdate(update, description)`

Send an update to all participants in the chat.

```javascript
webxdc.sendUpdate({
    payload: { score: 100 }
}, 'Updated score');
```

## Security

Mini Apps run in a highly restricted environment:

- **No network access**: Mini Apps cannot make HTTP requests or WebSocket connections
- **No WebRTC**: Peer-to-peer connections are disabled
- **No geolocation**: Location APIs are disabled
- **No camera/microphone**: Media capture is disabled
- **Strict CSP**: Content Security Policy prevents loading external resources

This ensures that Mini Apps are safe to run and cannot leak data.

## Building a .xdc File

To create a `.xdc` file, simply ZIP your app files:

```bash
cd my-app
zip -r ../my-app.xdc *
```

Make sure `index.html` is at the root of the archive, not in a subdirectory.

## Testing

You can test Mini Apps by:

1. Creating a `.xdc` file as described above
2. Sending it as a file attachment in a Vector chat
3. Clicking on the attachment to open the Mini App

## Examples

Check out these example Mini Apps:

- [WebXDC Examples](https://webxdc.org/apps/) - Official WebXDC app collection
- [Delta Chat WebXDC](https://github.com/DavidSM100/poll-webxdc) - Poll app example

## Compatibility

Vector's Mini Apps implementation is compatible with the WebXDC specification, so apps built for Delta Chat should work in Vector and vice versa.