CREATE TABLE IF NOT EXISTS milo_counts_daily (
    day date PRIMARY KEY,
    daily_active bigint NOT NULL DEFAULT 0 CHECK (daily_active >= 0),
    new_installs bigint NOT NULL DEFAULT 0 CHECK (new_installs >= 0)
);

CREATE TABLE IF NOT EXISTS milo_counts_monthly (
    month date PRIMARY KEY CHECK (month = date_trunc('month', month)::date),
    monthly_active bigint NOT NULL DEFAULT 0 CHECK (monthly_active >= 0)
);
