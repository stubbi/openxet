-- Running totals the daemon keeps, such as the bytes it has replaced by
-- clones. Losing them loses a statistic, never data.
CREATE TABLE counters (
    name TEXT PRIMARY KEY,
    value INTEGER NOT NULL
);
