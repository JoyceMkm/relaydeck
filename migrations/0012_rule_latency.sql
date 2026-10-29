-- Background TCP handshake latency per rule. Rows keep the probed target, so a
-- changed destination starts a new history instead of mixing two paths.
CREATE TABLE rule_latency_samples (
    rule_id INTEGER NOT NULL REFERENCES rules(id) ON DELETE CASCADE,
    target_ip TEXT NOT NULL,
    target_port INTEGER NOT NULL CHECK (target_port BETWEEN 1 AND 65535),
    observed_at INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('connected', 'timeout', 'refused', 'unreachable', 'retransmit')),
    latency_ms INTEGER CHECK ((status = 'connected') = (latency_ms IS NOT NULL) AND (latency_ms IS NULL OR latency_ms BETWEEN 0 AND 60000)),
    PRIMARY KEY (rule_id, observed_at)
) WITHOUT ROWID;
CREATE INDEX rule_latency_samples_time ON rule_latency_samples(observed_at);

-- Five-minute aggregates outlive raw samples for the weekly view.
CREATE TABLE rule_latency_buckets (
    rule_id INTEGER NOT NULL REFERENCES rules(id) ON DELETE CASCADE,
    target_ip TEXT NOT NULL,
    target_port INTEGER NOT NULL CHECK (target_port BETWEEN 1 AND 65535),
    bucket INTEGER NOT NULL,
    total INTEGER NOT NULL CHECK (total > 0),
    lost INTEGER NOT NULL CHECK (lost BETWEEN 0 AND total),
    valid INTEGER NOT NULL CHECK (valid = total - lost),
    sum_ms INTEGER NOT NULL CHECK (sum_ms >= 0),
    min_ms INTEGER,
    max_ms INTEGER,
    PRIMARY KEY (rule_id, target_ip, target_port, bucket)
) WITHOUT ROWID;
CREATE INDEX rule_latency_buckets_time ON rule_latency_buckets(bucket);
