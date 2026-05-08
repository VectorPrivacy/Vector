-- Add self-update tracking to groups table.
-- 0  = self-update required (post-join obligation per MIP-02)
-- >0 = unix timestamp of last successful self-update (periodic rotation per MIP-00)
--
-- Defaults to 0 (Required) for any existing groups, since we cannot know
-- whether they have performed a self-update before this migration.

ALTER TABLE groups ADD COLUMN last_self_update_at INTEGER NOT NULL DEFAULT 0;
