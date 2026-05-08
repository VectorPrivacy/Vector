# MDK Ruby Bindings Documentation

## Installation

Add to your `Gemfile`:

```ruby
gem 'mdk', '~> 0.5.2'
```

Then run:

```bash
bundle install
```

Or install directly:

```bash
gem install mdk
```

## Basic Usage

### Require and Initialize

```ruby
require 'mdk'

# Create an MDK instance with automatic key management (recommended)
# The encryption key is stored securely in your platform's keyring
db_path = "/path/to/mdk.db"
mdk = Mdk.new_mdk(db_path, "com.example.myapp", "mdk.db.key.default")

# For development only (unencrypted - never use in production!)
# mdk = Mdk.new_mdk_unencrypted(db_path)
```

### Create and Publish Key Package

```ruby
public_key = "your_hex_public_key"
relays = ["wss://relay.example.com", "wss://another-relay.com"]

result = mdk.create_key_package_for_event(
  public_key: public_key,
  relays: relays
)

# result.key_package contains the hex-encoded key package
# result.tags contains Nostr event tags (array of arrays)
# result.hash_ref contains the serialized hash reference for lifecycle tracking
# Publish as a Nostr event (kind 443) to your relays
```

### Parse Key Packages

```ruby
# When you receive a key package event from Nostr
event_json = <<~JSON
  {
    "id": "...",
    "kind": 443,
    "content": "hex_key_package...",
    ...
  }
JSON

mdk.parse_key_package(event_json: event_json)
```

### Create a Group

```ruby
creator_public_key = "your_hex_public_key"
member_key_package_events = ["{...}", "{...}"]  # JSON strings of key package events
name = "My Group"
description = "A secure group chat"
relays = ["wss://relay.example.com"]
admins = ["your_hex_public_key"]

result = mdk.create_group(
  creator_public_key: creator_public_key,
  member_key_package_events_json: member_key_package_events,
  name: name,
  description: description,
  relays: relays,
  admins: admins
)

# result.group contains the created group
# result.welcome_rumors_json contains welcome messages for initial members
```

### Get Groups

```ruby
groups = mdk.get_groups

groups.each do |group|
  puts "Group: #{group.name}"
  puts "ID: #{group.mls_group_id}"
  puts "State: #{group.state}"
  # To get member count, use: mdk.get_members(mls_group_id: group.mls_group_id).length
end
```

### Get a Specific Group

```ruby
group = mdk.get_group(mls_group_id: "hex_group_id")
if group
  puts "Found group: #{group.name}"
else
  puts "Group not found"
end
```

### Get Members

```ruby
members = mdk.get_members(mls_group_id: "hex_group_id")
puts "Group has #{members.length} members"
members.each do |member|
  puts "  - #{member}"
end
```

### Add Members to a Group

```ruby
mls_group_id = "hex_group_id"
key_package_events = ["{...}", "{...}"]  # JSON strings of key package events

result = mdk.add_members(
  mls_group_id: mls_group_id,
  key_package_events_json: key_package_events
)

# result.evolution_event_json contains the group update event
# result.welcome_rumors_json contains welcome messages for new members
```

### Remove Members from a Group

```ruby
mls_group_id = "hex_group_id"
member_public_keys = ["hex_pubkey1", "hex_pubkey2"]

result = mdk.remove_members(
  mls_group_id: mls_group_id,
  member_public_keys: member_public_keys
)
```

### Update Group Metadata

```ruby
mls_group_id = "hex_group_id"
new_name = "Updated Group Name"
new_description = "New description"
new_relays = ["wss://new-relay.com"]

result = mdk.update_group_metadata(
  mls_group_id: mls_group_id,
  name: new_name,
  description: new_description,
  relays: new_relays
)
```

### Accept Welcome Messages

```ruby
# Get pending welcomes
welcomes = mdk.get_pending_welcomes

welcomes.each do |welcome|
  puts "Invited to: #{welcome.group_name}"
  puts "By: #{welcome.welcomer}"

  # Accept the welcome
  mdk.accept_welcome(welcome_json: welcome.event_json)
end
```

### Decline Welcome Messages

```ruby
welcome = welcomes.first
mdk.decline_welcome(welcome_json: welcome.event_json)
```

### Create and Send Messages

```ruby
mls_group_id = "hex_group_id"
sender_public_key = "your_hex_public_key"
content = "Hello, group!"
kind = 9  # Message kind

event_json = mdk.create_message(
  mls_group_id: mls_group_id,
  sender_public_key: sender_public_key,
  content: content,
  kind: kind
)

# event_json is a JSON string of the encrypted Nostr event
# Publish this to your Nostr relays
```

### Get Messages

```ruby
messages = mdk.get_messages(mls_group_id: "hex_group_id")

messages.each do |message|
  puts "From: #{message.sender_pubkey}"
  puts "Event JSON: #{message.event_json}"
  puts "Kind: #{message.kind}"
  # Note: To extract decrypted content, parse the event_json and extract the content field
end
```

### Get a Specific Message

```ruby
message = mdk.get_message(event_id: "hex_event_id")
if message
  puts "Message event JSON: #{message.event_json}"
  # Note: To extract decrypted content, parse the event_json and extract the content field
end
```

### Process Incoming Messages

```ruby
# When you receive a message event from Nostr
event_json = <<~JSON
  {
    "id": "...",
    "kind": 1059,
    "content": "encrypted_content...",
    ...
  }
JSON

result = mdk.process_message(event_json: event_json)

case result
when MdkUniffi::MessageProcessingResult::NewMessage
  puts "New message event JSON: #{result.new_message.event_json}"
  # Note: To extract decrypted content, parse the event_json and extract the content field
when MdkUniffi::MessageProcessingResult::Duplicate
  puts "Message already processed"
when MdkUniffi::MessageProcessingResult::Error
  puts "Error processing: #{result.error}"
end
```

### Process Multiple Messages

```ruby
event_jsons = ["{...}", "{...}", "{...}"]
results = mdk.process_messages(event_jsons: event_jsons)

results.each do |result|
  if result.is_a?(MdkUniffi::MessageProcessingResult::NewMessage)
    puts "Processed message event JSON: #{result.new_message.event_json}"
    # Note: To extract decrypted content, parse the event_json and extract the content field
  end
end
```

## Error Handling

All MDK operations can raise `MdkUniffiError`:

```ruby
begin
  groups = mdk.get_groups
rescue MdkUniffi::MdkUniffiError::Storage => e
  puts "Storage error: #{e}"
rescue MdkUniffi::MdkUniffiError::Mdk => e
  puts "MDK error: #{e}"
rescue MdkUniffi::MdkUniffiError::InvalidInput => e
  puts "Invalid input: #{e}"
end
```

## Data Types

### Group

```ruby
# Group object with:
# - mls_group_id: String (hex-encoded MLS group ID)
# - nostr_group_id: String (hex-encoded Nostr group ID)
# - name: String
# - description: String
# - image_hash: Array<Byte> | nil (optional group image hash)
# - image_key: Array<Byte> | nil (optional group image encryption key)
# - image_nonce: Array<Byte> | nil (optional group image encryption nonce)
# - admin_pubkeys: Array<String> (admin public keys, hex-encoded)
# - last_message_id: String | nil (last message event ID, hex-encoded)
# - last_message_at: Integer | nil (timestamp of last message, Unix timestamp)
# - epoch: Integer (current epoch number)
# - state: String (group state, e.g., "active", "archived")
```

### Message

```ruby
# Message object with:
# - id: String (message ID, hex-encoded event ID)
# - mls_group_id: String (hex-encoded MLS group ID)
# - nostr_group_id: String (hex-encoded Nostr group ID)
# - event_id: String (event ID, hex-encoded)
# - sender_pubkey: String (sender public key, hex-encoded)
# - event_json: String (JSON representation of the event)
# - created_at: Integer (timestamp when message was created, Unix timestamp)
# - kind: Integer (message kind)
# - state: String (message state, e.g., "processed", "pending")
```

### Welcome

```ruby
# Welcome object with:
# - id: String (welcome ID, hex-encoded event ID)
# - event_json: String (JSON representation of the welcome event)
# - mls_group_id: String (hex-encoded MLS group ID)
# - nostr_group_id: String (hex-encoded Nostr group ID)
# - group_name: String
# - group_description: String
# - group_image_hash: Array<Byte> | nil (optional group image hash)
# - group_image_key: Array<Byte> | nil (optional group image encryption key)
# - group_image_nonce: Array<Byte> | nil (optional group image encryption nonce)
# - group_admin_pubkeys: Array<String> (list of admin public keys, hex-encoded)
# - group_relays: Array<String> (list of relay URLs for the group)
# - welcomer: String (welcomer public key, hex-encoded)
# - member_count: Integer (current member count)
# - state: String (welcome state, e.g., "pending", "accepted", "declined")
# - wrapper_event_id: String (wrapper event ID, hex-encoded)
```

### KeyPackageResult

```ruby
# KeyPackageResult object with:
# - key_package: String (hex-encoded key package)
# - tags: Array<Array<String>> (Nostr event tags)
```

## Thread Safety

A given `Mdk` instance must be confined to a single thread and must not be shared across threads. If you need to use MDK from multiple threads, create separate isolated `Mdk` instances per thread. Note that multi-threaded usage with separate instances is not a supported concurrency model.

## Example: Complete Workflow

```ruby
require 'mdk'

# 1. Initialize
db_path = "/path/to/mdk.db"
mdk = Mdk.new_mdk(db_path, "com.example.myapp", "mdk.db.key.default")

# 2. Create and publish key package
key_package = mdk.create_key_package_for_event(
  public_key: my_public_key,
  relays: ["wss://relay.example.com"]
)
# Publish key_package.key_package as Nostr event kind 443

# 3. Create a group
group_result = mdk.create_group(
  creator_public_key: my_public_key,
  member_key_package_events_json: [member_key_package_event_json],
  name: "My Group",
  description: "A test group",
  relays: ["wss://relay.example.com"],
  admins: [my_public_key]
)

# 4. Send a message
message_event = mdk.create_message(
  mls_group_id: group_result.group.mls_group_id,
  sender_public_key: my_public_key,
  content: "Hello!",
  kind: 9
)
# Publish message_event to Nostr relays

# 5. Retrieve messages
messages = mdk.get_messages(mls_group_id: group_result.group.mls_group_id)
messages.each do |message|
  puts "#{message.sender_pubkey}: #{message.event_json}"
  # Note: To extract decrypted content, parse the event_json and extract the content field
end
```

## Integration with Nostr Libraries

```ruby
require 'nostr'
require 'mdk'

# Initialize Nostr client
keys = Nostr::KeyPair.generate
client = Nostr::Client.new(keys)

# Create key package and publish
key_package = mdk.create_key_package_for_event(
  public_key: keys.public_key.to_hex,
  relays: ["wss://relay.example.com"]
)

event = Nostr::Event.new(
  kind: 443,
  content: key_package.key_package,
  tags: key_package.tags
)
client.publish(event)
```

