-- Add processed_at column to messages table
-- This timestamp records when this client processed/received the message,
-- which is useful for consistent message ordering across devices with clock skew.
-- For existing rows, we default to created_at as the best available approximation.
ALTER TABLE messages ADD COLUMN processed_at INTEGER;
UPDATE messages SET processed_at = created_at WHERE processed_at IS NULL;

-- Create a composite index for the new sort order (created_at DESC, processed_at DESC, id DESC)
-- This ensures stable ordering: by sender's timestamp, then by reception time, then by id for determinism
CREATE INDEX IF NOT EXISTS idx_messages_sorting ON messages(mls_group_id, created_at DESC, processed_at DESC, id DESC);

-- Add last_message_processed_at column to groups table
-- This column stores when the last message was processed/received by this client,
-- enabling consistent message ordering between group.last_message_id and get_messages()[0].

ALTER TABLE groups ADD COLUMN last_message_processed_at INTEGER;

-- Backfill existing rows with last_message_at as a reasonable default
UPDATE groups SET last_message_processed_at = last_message_at WHERE last_message_processed_at IS NULL AND last_message_at IS NOT NULL;
