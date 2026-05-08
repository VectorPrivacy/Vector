# MDK Swift Bindings Documentation

## Installation

Add the MDK package to your `Package.swift`:

```swift
dependencies: [
    .package(url: "https://github.com/marmot-protocol/mdk-swift.git", from: "0.5.2")
]
```

Or add it via Xcode: File → Add Packages → Enter the repository URL.

## Basic Usage

### Import and Initialize

```swift
import MDKBindings

// Create an MDK instance with a SQLite database path
let dbPath = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
    .appendingPathComponent("mdk.db").path

let mdk = try newMdk(dbPath: dbPath)
```

### Create and Publish Key Package

```swift
let publicKey = "your_hex_public_key"
let relays = ["wss://relay.example.com", "wss://another-relay.com"]

let result = try mdk.createKeyPackageForEvent(
    publicKey: publicKey,
    relays: relays
)

// result.keyPackage contains the hex-encoded key package
// result.tags contains Nostr event tags
// Publish as a Nostr event (kind 443) to your relays
```

### Parse Key Packages

```swift
// When you receive a key package event from Nostr
let eventJson = """
{
    "id": "...",
    "kind": 443,
    "content": "hex_key_package...",
    ...
}
"""

try mdk.parseKeyPackage(eventJson: eventJson)
```

### Create a Group

```swift
let creatorPublicKey = "your_hex_public_key"
let memberKeyPackageEvents = ["{...}", "{...}"] // JSON strings of key package events
let name = "My Group"
let description = "A secure group chat"
let relays = ["wss://relay.example.com"]
let admins = ["your_hex_public_key"]

let result = try mdk.createGroup(
    creatorPublicKey: creatorPublicKey,
    memberKeyPackageEventsJson: memberKeyPackageEvents,
    name: name,
    description: description,
    relays: relays,
    admins: admins
)

// result.group contains the created group
// result.welcomeRumorsJson contains welcome messages for initial members
```

### Get Groups

```swift
let groups = try mdk.getGroups()

for group in groups {
    print("Group: \(group.name)")
    print("ID: \(group.mlsGroupId)")
    print("State: \(group.state)")
    // To get member count, use: try mdk.getMembers(mlsGroupId: group.mlsGroupId).count
}
```

### Get a Specific Group

```swift
if let group = try mdk.getGroup(mlsGroupId: "hex_group_id") {
    print("Found group: \(group.name)")
} else {
    print("Group not found")
}
```

### Add Members to a Group

```swift
let mlsGroupId = "hex_group_id"
let keyPackageEvents = ["{...}", "{...}"] // JSON strings of key package events

let result = try mdk.addMembers(
    mlsGroupId: mlsGroupId,
    keyPackageEventsJson: keyPackageEvents
)

// result.evolutionEventJson contains the group update event
// result.welcomeRumorsJson contains welcome messages for new members
```

### Remove Members from a Group

```swift
let mlsGroupId = "hex_group_id"
let memberPublicKeys = ["hex_pubkey1", "hex_pubkey2"]

let result = try mdk.removeMembers(
    mlsGroupId: mlsGroupId,
    memberPublicKeys: memberPublicKeys
)
```

### Accept Welcome Messages

```swift
// Get pending welcomes
let welcomes = try mdk.getPendingWelcomes()

for welcome in welcomes {
    print("Invited to: \(welcome.groupName)")
    print("By: \(welcome.welcomer)")
    
    // Accept the welcome
    try mdk.acceptWelcome(welcomeJson: welcome.eventJson)
}
```

### Create and Send Messages

```swift
let mlsGroupId = "hex_group_id"
let senderPublicKey = "your_hex_public_key"
let content = "Hello, group!"
let kind: UInt16 = 9 // Message kind

let eventJson = try mdk.createMessage(
    mlsGroupId: mlsGroupId,
    senderPublicKey: senderPublicKey,
    content: content,
    kind: kind
)

// eventJson is a JSON string of the encrypted Nostr event
// Publish this to your Nostr relays
```

### Get Messages

```swift
let messages = try mdk.getMessages(mlsGroupId: "hex_group_id")

for message in messages {
    print("From: \(message.senderPubkey)")
    print("Event JSON: \(message.eventJson)")
    print("Kind: \(message.kind)")
    // Note: To extract decrypted content, parse the eventJson and extract the content field
}
```

### Process Incoming Messages

```swift
// When you receive a message event from Nostr
let eventJson = """
{
    "id": "...",
    "kind": 1059,
    "content": "encrypted_content...",
    ...
}
"""

let result = try mdk.processMessage(eventJson: eventJson)

switch result {
case .newMessage(let message):
    print("New message event JSON: \(message.eventJson)")
    // Note: To extract decrypted content, parse the eventJson and extract the content field
case .duplicate:
    print("Message already processed")
case .error(let error):
    print("Error processing: \(error)")
}
```

## Error Handling

All MDK operations can throw `MdkUniffiError`:

```swift
do {
    let groups = try mdk.getGroups()
    // Use groups...
} catch MdkUniffiError.storage(let message) {
    print("Storage error: \(message)")
} catch MdkUniffiError.mdk(let message) {
    print("MDK error: \(message)")
} catch MdkUniffiError.invalidInput(let message) {
    print("Invalid input: \(message)")
} catch {
    print("Unexpected error: \(error)")
}
```

## Data Types

### Group

```swift
struct Group {
    let mlsGroupId: String              // Hex-encoded MLS group ID
    let nostrGroupId: String            // Hex-encoded Nostr group ID
    let name: String
    let description: String
    let imageHash: [UInt8]?            // Optional group image hash
    let imageKey: [UInt8]?             // Optional group image encryption key
    let imageNonce: [UInt8]?           // Optional group image encryption nonce
    let adminPubkeys: [String]          // Admin public keys (hex-encoded)
    let lastMessageId: String?         // Last message event ID (hex-encoded)
    let lastMessageAt: UInt64?          // Timestamp of last message (Unix timestamp)
    let epoch: UInt64                   // Current epoch number
    let state: String                   // Group state (e.g., "active", "archived")
}
```

### Message

```swift
struct Message {
    let id: String                     // Message ID (hex-encoded event ID)
    let mlsGroupId: String             // Hex-encoded MLS group ID
    let nostrGroupId: String           // Hex-encoded Nostr group ID
    let eventId: String                // Event ID (hex-encoded)
    let senderPubkey: String           // Sender public key (hex-encoded)
    let eventJson: String              // JSON representation of the event
    let processedAt: UInt64            // Timestamp when message was processed (Unix timestamp)
    let kind: UInt16                   // Message kind
    let state: String                  // Message state (e.g., "processed", "pending")
}
```

### Welcome

```swift
struct Welcome {
    let id: String                     // Welcome ID (hex-encoded event ID)
    let eventJson: String              // JSON representation of the welcome event
    let mlsGroupId: String             // Hex-encoded MLS group ID
    let nostrGroupId: String           // Hex-encoded Nostr group ID
    let groupName: String
    let groupDescription: String
    let groupImageHash: [UInt8]?       // Optional group image hash
    let groupImageKey: [UInt8]?        // Optional group image encryption key
    let groupImageNonce: [UInt8]?      // Optional group image encryption nonce
    let groupAdminPubkeys: [String]    // List of admin public keys (hex-encoded)
    let groupRelays: [String]          // List of relay URLs for the group
    let welcomer: String               // Welcomer public key (hex-encoded)
    let memberCount: UInt32            // Current member count
    let state: String                  // Welcome state (e.g., "pending", "accepted", "declined")
    let wrapperEventId: String         // Wrapper event ID (hex-encoded)
}
```

### KeyPackageResult

```swift
struct KeyPackageResult {
    let keyPackage: String        // Hex-encoded key package
    let tags: [[String]]          // Nostr event tags
}
```

## Thread Safety

A given `Mdk` instance must be confined to a single thread and must not be shared across threads. If you need to use MDK from multiple threads, create separate isolated `Mdk` instances per thread. Note that multi-threaded usage with separate instances is not a supported concurrency model.

## iOS Integration

The Swift package includes an XCFramework that supports both iOS device and simulator. The package automatically links against `sqlite3` and `c++` as required.

## Example: Complete Workflow

```swift
import MDKBindings

// 1. Initialize
let dbPath = "/path/to/mdk.db"
let mdk = try newMdk(dbPath: dbPath)

// 2. Create and publish key package
let keyPackage = try mdk.createKeyPackageForEvent(
    publicKey: myPublicKey,
    relays: ["wss://relay.example.com"]
)
// Publish keyPackage.keyPackage as Nostr event kind 443

// 3. Create a group
let group = try mdk.createGroup(
    creatorPublicKey: myPublicKey,
    memberKeyPackageEventsJson: [memberKeyPackageEventJson],
    name: "My Group",
    description: "A test group",
    relays: ["wss://relay.example.com"],
    admins: [myPublicKey]
)

// 4. Send a message
let messageEvent = try mdk.createMessage(
    mlsGroupId: group.group.mlsGroupId,
    senderPublicKey: myPublicKey,
    content: "Hello!",
    kind: 9
)
// Publish messageEvent to Nostr relays

// 5. Retrieve messages
let messages = try mdk.getMessages(mlsGroupId: group.group.mlsGroupId)
for message in messages {
    print("\(message.senderPubkey): \(message.eventJson)")
    // Note: To extract decrypted content, parse the eventJson and extract the content field
}
```

