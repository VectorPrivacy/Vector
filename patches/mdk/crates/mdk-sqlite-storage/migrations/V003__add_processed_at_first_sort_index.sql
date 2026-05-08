-- Add a composite index for the processed_at-first sort order
-- (processed_at DESC, created_at DESC, id DESC) scoped to mls_group_id.
-- This allows efficient queries when clients request MessageSortOrder::ProcessedAtFirst,
-- which prioritises local reception time over the sender's timestamp.
CREATE INDEX IF NOT EXISTS idx_messages_sorting_processed_at
  ON messages(mls_group_id, processed_at DESC, created_at DESC, id DESC);
