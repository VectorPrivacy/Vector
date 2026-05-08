-- Unified initial schema for MDK SQLite Storage
-- This schema combines both OpenMLS storage tables and MDK-specific tables
-- in a single migration to enable atomic transactions across all MLS state.

-- ============================================================================
-- OpenMLS Storage Tables
-- ============================================================================

-- Group data: polymorphic storage for various group-related data types
CREATE TABLE IF NOT EXISTS openmls_group_data (
    provider_version INTEGER NOT NULL,
    group_id BLOB NOT NULL,
    data_type TEXT NOT NULL CHECK (data_type IN (
        'join_group_config',
        'tree',
        'interim_transcript_hash',
        'context',
        'confirmation_tag',
        'group_state',
        'message_secrets',
        'resumption_psk_store',
        'own_leaf_index',
        'group_epoch_secrets'
    )),
    group_data BLOB NOT NULL,
    PRIMARY KEY (group_id, data_type)
);

-- Proposals: queued proposals for groups
CREATE TABLE IF NOT EXISTS openmls_proposals (
    provider_version INTEGER NOT NULL,
    group_id BLOB NOT NULL,
    proposal_ref BLOB NOT NULL,
    proposal BLOB NOT NULL,
    PRIMARY KEY (group_id, proposal_ref)
);

-- Own leaf nodes: list of own leaf nodes per group
CREATE TABLE IF NOT EXISTS openmls_own_leaf_nodes (
    provider_version INTEGER NOT NULL,
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    group_id BLOB NOT NULL,
    leaf_node BLOB NOT NULL
);

-- Key packages: stored key packages indexed by hash reference
CREATE TABLE IF NOT EXISTS openmls_key_packages (
    provider_version INTEGER NOT NULL,
    key_package_ref BLOB PRIMARY KEY,
    key_package BLOB NOT NULL
);

-- PSKs: pre-shared keys indexed by PSK ID
CREATE TABLE IF NOT EXISTS openmls_psks (
    provider_version INTEGER NOT NULL,
    psk_id BLOB PRIMARY KEY,
    psk_bundle BLOB NOT NULL
);

-- Signature keys: signature key pairs indexed by public key
CREATE TABLE IF NOT EXISTS openmls_signature_keys (
    provider_version INTEGER NOT NULL,
    public_key BLOB PRIMARY KEY,
    signature_key BLOB NOT NULL
);

-- Encryption keys: HPKE key pairs indexed by public key
CREATE TABLE IF NOT EXISTS openmls_encryption_keys (
    provider_version INTEGER NOT NULL,
    public_key BLOB PRIMARY KEY,
    key_pair BLOB NOT NULL
);

-- Epoch key pairs: HPKE key pairs for specific epochs
CREATE TABLE IF NOT EXISTS openmls_epoch_key_pairs (
    provider_version INTEGER NOT NULL,
    group_id BLOB NOT NULL,
    epoch_id BLOB NOT NULL,
    leaf_index INTEGER NOT NULL,
    key_pairs BLOB NOT NULL,
    PRIMARY KEY (group_id, epoch_id, leaf_index)
);

-- ============================================================================
-- MDK Storage Tables
-- ============================================================================

-- Groups: MDK group metadata
CREATE TABLE IF NOT EXISTS groups (
    mls_group_id BLOB PRIMARY KEY,
    nostr_group_id BLOB NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    admin_pubkeys JSONB NOT NULL,
    last_message_id BLOB,
    last_message_at INTEGER,
    epoch INTEGER NOT NULL,
    state TEXT NOT NULL,
    image_hash BLOB,
    image_key BLOB,
    image_nonce BLOB
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_groups_nostr_group_id ON groups(nostr_group_id);

-- Group relays: relay URLs associated with groups
CREATE TABLE IF NOT EXISTS group_relays (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    mls_group_id BLOB NOT NULL,
    relay_url TEXT NOT NULL,
    FOREIGN KEY (mls_group_id) REFERENCES groups(mls_group_id) ON DELETE CASCADE,
    UNIQUE(mls_group_id, relay_url)
);

CREATE INDEX IF NOT EXISTS idx_group_relays_mls_group_id ON group_relays(mls_group_id);

-- Group exporter secrets: epoch-specific secrets for groups
CREATE TABLE IF NOT EXISTS group_exporter_secrets (
    mls_group_id BLOB NOT NULL,
    epoch INTEGER NOT NULL,
    secret BLOB NOT NULL,
    PRIMARY KEY (mls_group_id, epoch),
    FOREIGN KEY (mls_group_id) REFERENCES groups(mls_group_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_group_exporter_secrets_mls_group_id ON group_exporter_secrets(mls_group_id);

-- Messages: decrypted MLS messages
CREATE TABLE IF NOT EXISTS messages (
    mls_group_id BLOB NOT NULL,
    id BLOB NOT NULL,
    pubkey BLOB NOT NULL,
    kind INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    content TEXT NOT NULL,
    tags JSONB NOT NULL,
    event JSONB NOT NULL,
    wrapper_event_id BLOB NOT NULL,
    state TEXT NOT NULL,
    epoch INTEGER,
    PRIMARY KEY (mls_group_id, id),
    FOREIGN KEY (mls_group_id) REFERENCES groups(mls_group_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_messages_mls_group_id ON messages(mls_group_id);
CREATE INDEX IF NOT EXISTS idx_messages_wrapper_event_id ON messages(wrapper_event_id);
CREATE INDEX IF NOT EXISTS idx_messages_created_at ON messages(created_at);
CREATE INDEX IF NOT EXISTS idx_messages_pubkey ON messages(pubkey);
CREATE INDEX IF NOT EXISTS idx_messages_kind ON messages(kind);
CREATE INDEX IF NOT EXISTS idx_messages_state ON messages(state);
CREATE INDEX IF NOT EXISTS idx_messages_epoch ON messages(mls_group_id, epoch);

-- Processed messages: tracking of processed wrapper events
CREATE TABLE IF NOT EXISTS processed_messages (
    wrapper_event_id BLOB PRIMARY KEY,
    message_event_id BLOB,
    processed_at INTEGER NOT NULL,
    epoch INTEGER,
    mls_group_id BLOB,
    state TEXT NOT NULL,
    failure_reason TEXT
);

CREATE INDEX IF NOT EXISTS idx_processed_messages_message_event_id ON processed_messages(message_event_id);
CREATE INDEX IF NOT EXISTS idx_processed_messages_state ON processed_messages(state);
CREATE INDEX IF NOT EXISTS idx_processed_messages_processed_at ON processed_messages(processed_at);
CREATE INDEX IF NOT EXISTS idx_processed_messages_epoch ON processed_messages(mls_group_id, epoch);

-- Welcomes: pending welcome messages
CREATE TABLE IF NOT EXISTS welcomes (
    id BLOB PRIMARY KEY,
    event JSONB NOT NULL,
    mls_group_id BLOB NOT NULL,
    nostr_group_id BLOB NOT NULL,
    group_name TEXT NOT NULL,
    group_description TEXT NOT NULL,
    group_admin_pubkeys JSONB NOT NULL,
    group_relays JSONB NOT NULL,
    welcomer BLOB NOT NULL,
    member_count INTEGER NOT NULL,
    state TEXT NOT NULL,
    wrapper_event_id BLOB NOT NULL,
    group_image_hash BLOB,
    group_image_key BLOB,
    group_image_nonce BLOB
);

CREATE INDEX IF NOT EXISTS idx_welcomes_mls_group_id ON welcomes(mls_group_id);
CREATE INDEX IF NOT EXISTS idx_welcomes_wrapper_event_id ON welcomes(wrapper_event_id);
CREATE INDEX IF NOT EXISTS idx_welcomes_state ON welcomes(state);
CREATE INDEX IF NOT EXISTS idx_welcomes_nostr_group_id ON welcomes(nostr_group_id);

-- Processed welcomes: tracking of processed welcome events
CREATE TABLE IF NOT EXISTS processed_welcomes (
    wrapper_event_id BLOB PRIMARY KEY,
    welcome_event_id BLOB,
    processed_at INTEGER NOT NULL,
    state TEXT NOT NULL,
    failure_reason TEXT
);

CREATE INDEX IF NOT EXISTS idx_processed_welcomes_welcome_event_id ON processed_welcomes(welcome_event_id);
CREATE INDEX IF NOT EXISTS idx_processed_welcomes_state ON processed_welcomes(state);
CREATE INDEX IF NOT EXISTS idx_processed_welcomes_processed_at ON processed_welcomes(processed_at);

-- ============================================================================
-- Group State Snapshots (MIP-03 Commit Race Resolution)
-- ============================================================================
-- Instead of SQLite savepoints (which have stack-ordering issues where
-- releasing an old savepoint destroys all newer ones), we copy group-specific
-- rows to this table for later restoration.

CREATE TABLE IF NOT EXISTS group_state_snapshots (
    snapshot_name TEXT NOT NULL,
    group_id BLOB NOT NULL,
    table_name TEXT NOT NULL,
    row_key BLOB NOT NULL,      -- Serialized primary key (JSON)
    row_data BLOB NOT NULL,     -- Serialized row data (JSON)
    created_at INTEGER NOT NULL,
    PRIMARY KEY (snapshot_name, group_id, table_name, row_key),
    FOREIGN KEY (group_id) REFERENCES groups(mls_group_id) ON DELETE CASCADE
);
