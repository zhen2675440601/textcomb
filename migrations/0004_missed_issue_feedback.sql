CREATE TABLE missed_issue_feedback (
    id UUID PRIMARY KEY,
    report_id UUID NOT NULL REFERENCES reports(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    category VARCHAR(24) NOT NULL CHECK (category IN ('typo', 'punctuation', 'grammar', 'paragraph')),
    char_start INTEGER NOT NULL,
    char_end INTEGER NOT NULL CHECK (char_end > char_start),
    quote TEXT NOT NULL,
    note TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX missed_issue_feedback_report_idx ON missed_issue_feedback(report_id, created_at);
