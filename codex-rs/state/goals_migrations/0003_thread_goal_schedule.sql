ALTER TABLE thread_goals ADD COLUMN timezone TEXT;
ALTER TABLE thread_goals ADD COLUMN stages_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE thread_goals ADD COLUMN initial_quota_snapshots_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE thread_goals ADD COLUMN initial_token_budget INTEGER;
ALTER TABLE thread_goals ADD COLUMN baseline_captured INTEGER NOT NULL DEFAULT 0;
