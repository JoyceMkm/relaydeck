//! Background TCP handshake latency from this host to each running rule's target.
//!
//! The root broker probes on its own clock as the tenant's UID, so the kernel
//! egress policy limits every probe to that tenant's applied targets. The worker
//! drains the samples into SQLite; the API serves them read-only.
use crate::connectivity::TcpCheck;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::{
    future::Future,
    net::{IpAddr, SocketAddr},
};

pub const PROBE_INTERVAL_SECS: u64 = 60;
pub const SAMPLE_RETENTION_SECS: i64 = 24 * 60 * 60;
pub const BUCKET_SECS: i64 = 5 * 60;
pub const BUCKET_RETENTION_SECS: i64 = 7 * 24 * 60 * 60;
/// Samples per broker reply; keeps a frame of IPv6 samples far below the broker frame limit.
pub const PAGE: usize = 400;
pub const MAX_LATENCY_MS: u32 = 60_000;
// Following Komari's tcping: a handshake slower than one SYN retransmission
// timeout is measured again. A retry that is much faster means the first SYN
// was lost, which is reported as loss rather than as a one-second spike.
const HIGH_LATENCY_MS: u64 = 1000;
const RETRANSMIT_DROP_MS: u64 = 800;
const HIGH_LATENCY_RETRIES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum ProbeStatus {
    Connected,
    Timeout,
    Refused,
    Unreachable,
    Retransmit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    pub status: ProbeStatus,
    pub latency_ms: Option<u32>,
}

impl Probe {
    /// A latency exists exactly for a completed handshake, within the stored bound.
    pub fn valid(&self) -> bool {
        (self.status == ProbeStatus::Connected) == self.latency_ms.is_some()
            && self.latency_ms.is_none_or(|ms| ms <= MAX_LATENCY_MS)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sample {
    /// Runtime slot, as used on the broker socket.
    pub owner: i64,
    pub revision: i64,
    pub rule_id: i64,
    pub target_ip: IpAddr,
    pub target_port: u16,
    pub observed_at: i64,
    pub status: ProbeStatus,
    pub latency_ms: Option<u32>,
}

impl Sample {
    pub fn probe(&self) -> Probe {
        Probe {
            status: self.status,
            latency_ms: self.latency_ms,
        }
    }
}

fn connected(check: &TcpCheck) -> Option<u64> {
    (check.status == "connected").then_some(check.elapsed_ms)
}

fn failure(check: &TcpCheck) -> Probe {
    let status = match check.status.as_str() {
        "timeout" => ProbeStatus::Timeout,
        "refused" => ProbeStatus::Refused,
        _ => ProbeStatus::Unreachable,
    };
    Probe {
        status,
        latency_ms: None,
    }
}

fn reached(elapsed_ms: u64) -> Probe {
    Probe {
        status: ProbeStatus::Connected,
        latency_ms: Some(elapsed_ms.min(u64::from(MAX_LATENCY_MS)) as u32),
    }
}

/// One latency sample using `measure` for each handshake attempt.
pub async fn probe_with<F, Fut>(mut measure: F) -> Probe
where
    F: FnMut() -> Fut,
    Fut: Future<Output = TcpCheck>,
{
    let first = measure().await;
    let Some(first_ms) = connected(&first) else {
        return failure(&first);
    };
    if first_ms <= HIGH_LATENCY_MS {
        return reached(first_ms);
    }
    let mut lowest = first_ms;
    for _ in 0..HIGH_LATENCY_RETRIES {
        let next = measure().await;
        let Some(ms) = connected(&next) else {
            return failure(&next);
        };
        if ms <= HIGH_LATENCY_MS {
            if first_ms - ms > RETRANSMIT_DROP_MS {
                return Probe {
                    status: ProbeStatus::Retransmit,
                    latency_ms: None,
                };
            }
            return reached(ms);
        }
        lowest = lowest.min(ms);
    }
    // Unlike Komari, a consistently slow path is still reachable: report it.
    reached(lowest)
}

/// Probes every address concurrently and returns the results in input order.
pub async fn probe_all(addresses: &[SocketAddr]) -> Vec<Probe> {
    let mut tasks = tokio::task::JoinSet::new();
    for (index, address) in addresses.iter().copied().enumerate() {
        tasks.spawn(async move {
            (
                index,
                probe_with(|| crate::connectivity::tcp(address)).await,
            )
        });
    }
    let mut results = vec![
        Probe {
            status: ProbeStatus::Unreachable,
            latency_ms: None,
        };
        addresses.len()
    ];
    while let Some(joined) = tasks.join_next().await {
        if let Ok((index, probe)) = joined {
            results[index] = probe;
        }
    }
    results
}

/// Stores samples whose rule still has the probed revision and target. Others
/// belong to a configuration that changed while the probe was running.
pub async fn record(pool: &SqlitePool, samples: &[Sample]) -> anyhow::Result<usize> {
    if samples.is_empty() {
        return Ok(0);
    }
    let timestamp = crate::db::now();
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let mut stored = 0;
    for sample in samples {
        if !sample.probe().valid()
            || sample.observed_at > timestamp + 60
            || sample.observed_at <= timestamp - SAMPLE_RETENTION_SECS
        {
            tracing::warn!(
                rule_id = sample.rule_id,
                "discarding malformed latency sample"
            );
            continue;
        }
        let target_ip = sample.target_ip.to_string();
        let latency = sample.latency_ms.map(i64::from);
        let inserted = sqlx::query("INSERT OR IGNORE INTO rule_latency_samples(rule_id,target_ip,target_port,observed_at,status,latency_ms) SELECT r.id,r.target_ip,r.target_port,?,?,? FROM rules r JOIN users u ON u.id=r.owner_id WHERE r.id=? AND COALESCE(u.runtime_slot,u.id)=? AND u.desired_revision=? AND r.deleted_at IS NULL AND r.target_ip=? AND r.target_port=?")
            .bind(sample.observed_at).bind(sample.status).bind(latency)
            .bind(sample.rule_id).bind(sample.owner).bind(sample.revision).bind(&target_ip).bind(i64::from(sample.target_port))
            .execute(&mut *tx).await?;
        if inserted.rows_affected() == 0 {
            continue;
        }
        let bucket = sample.observed_at - sample.observed_at.rem_euclid(BUCKET_SECS);
        sqlx::query("INSERT INTO rule_latency_buckets(rule_id,target_ip,target_port,bucket,total,lost,valid,sum_ms,min_ms,max_ms) VALUES(?,?,?,?,1,?,?,?,?,?) ON CONFLICT(rule_id,target_ip,target_port,bucket) DO UPDATE SET total=total+1,lost=lost+excluded.lost,valid=valid+excluded.valid,sum_ms=sum_ms+excluded.sum_ms,min_ms=COALESCE(MIN(min_ms,excluded.min_ms),min_ms,excluded.min_ms),max_ms=COALESCE(MAX(max_ms,excluded.max_ms),max_ms,excluded.max_ms)")
            .bind(sample.rule_id).bind(&target_ip).bind(i64::from(sample.target_port)).bind(bucket)
            .bind(i64::from(latency.is_none())).bind(i64::from(latency.is_some())).bind(latency.unwrap_or(0)).bind(latency).bind(latency)
            .execute(&mut *tx).await?;
        stored += 1;
    }
    tx.commit().await?;
    Ok(stored)
}

pub async fn prune(pool: &SqlitePool, timestamp: i64) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM rule_latency_samples WHERE observed_at<=?")
        .bind(timestamp - SAMPLE_RETENTION_SECS)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM rule_latency_buckets WHERE bucket<=?")
        .bind(timestamp - BUCKET_RETENTION_SECS)
        .execute(pool)
        .await?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Range {
    #[serde(rename = "1h")]
    Hour,
    #[serde(rename = "24h")]
    Day,
    #[serde(rename = "7d")]
    Week,
}

impl Range {
    fn seconds(self) -> i64 {
        match self {
            Self::Hour => 60 * 60,
            Self::Day => 24 * 60 * 60,
            Self::Week => 7 * 24 * 60 * 60,
        }
    }
    /// Width of one point: raw samples, five-minute buckets, then hours.
    fn step(self) -> i64 {
        match self {
            Self::Hour => PROBE_INTERVAL_SECS as i64,
            Self::Day => BUCKET_SECS,
            Self::Week => 60 * 60,
        }
    }
}

#[derive(Debug, Serialize, PartialEq, sqlx::FromRow)]
pub struct Point {
    pub at: i64,
    pub total: i64,
    pub lost: i64,
    pub avg_ms: Option<i64>,
    pub min_ms: Option<i64>,
    pub max_ms: Option<i64>,
}

#[derive(Debug, Default, Serialize, PartialEq)]
pub struct Summary {
    pub total: i64,
    pub lost: i64,
    pub loss_percent: Option<f64>,
    pub avg_ms: Option<i64>,
    pub min_ms: Option<i64>,
    pub max_ms: Option<i64>,
    /// Only where raw samples cover the range (one hour and one day).
    pub p50_ms: Option<i64>,
    pub p99_ms: Option<i64>,
}

#[derive(Debug, Serialize, PartialEq, sqlx::FromRow)]
pub struct Latest {
    pub observed_at: i64,
    pub status: ProbeStatus,
    pub latency_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct History {
    pub rule_id: i64,
    pub target_ip: String,
    pub target_port: i64,
    /// UDP has no handshake to time; only TCP rules are probed.
    pub supported: bool,
    pub interval_secs: u64,
    pub step_secs: i64,
    pub from: i64,
    pub to: i64,
    pub latest: Option<Latest>,
    pub summary: Summary,
    pub points: Vec<Point>,
}

/// Nearest-rank percentile of sorted values.
fn percentile(sorted: &[i64], fraction: f64) -> Option<i64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (fraction * sorted.len() as f64).ceil() as usize;
    Some(sorted[rank.clamp(1, sorted.len()) - 1])
}

fn loss_percent(total: i64, lost: i64) -> Option<f64> {
    (total > 0).then(|| (lost as f64 * 1000.0 / total as f64).round() / 10.0)
}

fn summarize(raw: &[Option<i64>]) -> Summary {
    let mut values: Vec<i64> = raw.iter().flatten().copied().collect();
    values.sort_unstable();
    let total = raw.len() as i64;
    let lost = total - values.len() as i64;
    Summary {
        total,
        lost,
        loss_percent: loss_percent(total, lost),
        avg_ms: (!values.is_empty())
            .then(|| (values.iter().sum::<i64>() as f64 / values.len() as f64).round() as i64),
        min_ms: values.first().copied(),
        max_ms: values.last().copied(),
        p50_ms: percentile(&values, 0.5),
        p99_ms: percentile(&values, 0.99),
    }
}

/// History of a rule's current target. The caller has already authorized the rule.
pub async fn history(
    pool: &SqlitePool,
    rule_id: i64,
    target_ip: &str,
    target_port: i64,
    supported: bool,
    range: Range,
    timestamp: i64,
) -> Result<History, sqlx::Error> {
    let from = timestamp - range.seconds();
    let latest: Option<Latest> = sqlx::query_as("SELECT observed_at,status,latency_ms FROM rule_latency_samples WHERE rule_id=? AND target_ip=? AND target_port=? ORDER BY observed_at DESC LIMIT 1")
        .bind(rule_id).bind(target_ip).bind(target_port).fetch_optional(pool).await?;
    let (summary, points) = match range {
        Range::Hour | Range::Day => {
            let raw: Vec<(i64, Option<i64>)> = sqlx::query_as("SELECT observed_at,latency_ms FROM rule_latency_samples WHERE rule_id=? AND target_ip=? AND target_port=? AND observed_at>? ORDER BY observed_at")
                .bind(rule_id).bind(target_ip).bind(target_port).bind(from).fetch_all(pool).await?;
            let summary = summarize(&raw.iter().map(|(_, ms)| *ms).collect::<Vec<_>>());
            let points = if range == Range::Hour {
                raw.into_iter()
                    .map(|(at, ms)| Point {
                        at,
                        total: 1,
                        lost: i64::from(ms.is_none()),
                        avg_ms: ms,
                        min_ms: ms,
                        max_ms: ms,
                    })
                    .collect()
            } else {
                buckets(pool, rule_id, target_ip, target_port, from, range.step()).await?
            };
            (summary, points)
        }
        Range::Week => {
            let (total, lost, sum, valid, min_ms, max_ms): (i64, i64, i64, i64, Option<i64>, Option<i64>) = sqlx::query_as("SELECT COALESCE(SUM(total),0),COALESCE(SUM(lost),0),COALESCE(SUM(sum_ms),0),COALESCE(SUM(valid),0),MIN(min_ms),MAX(max_ms) FROM rule_latency_buckets WHERE rule_id=? AND target_ip=? AND target_port=? AND bucket>?")
                .bind(rule_id).bind(target_ip).bind(target_port).bind(from - BUCKET_SECS).fetch_one(pool).await?;
            let summary = Summary {
                total,
                lost,
                loss_percent: loss_percent(total, lost),
                avg_ms: (valid > 0).then(|| (sum as f64 / valid as f64).round() as i64),
                min_ms,
                max_ms,
                p50_ms: None,
                p99_ms: None,
            };
            let points = buckets(pool, rule_id, target_ip, target_port, from, range.step()).await?;
            (summary, points)
        }
    };
    Ok(History {
        rule_id,
        target_ip: target_ip.into(),
        target_port,
        supported,
        interval_secs: PROBE_INTERVAL_SECS,
        step_secs: range.step(),
        from,
        to: timestamp,
        latest,
        summary,
        points,
    })
}

async fn buckets(
    pool: &SqlitePool,
    rule_id: i64,
    target_ip: &str,
    target_port: i64,
    from: i64,
    step: i64,
) -> Result<Vec<Point>, sqlx::Error> {
    sqlx::query_as("SELECT bucket/?*? AS at,SUM(total) AS total,SUM(lost) AS lost,CASE WHEN SUM(valid)>0 THEN CAST(ROUND(1.0*SUM(sum_ms)/SUM(valid)) AS INTEGER) END AS avg_ms,MIN(min_ms) AS min_ms,MAX(max_ms) AS max_ms FROM rule_latency_buckets WHERE rule_id=? AND target_ip=? AND target_port=? AND bucket>? GROUP BY bucket/? ORDER BY at")
        .bind(step).bind(step).bind(rule_id).bind(target_ip).bind(target_port).bind(from - BUCKET_SECS).bind(step)
        .fetch_all(pool).await
}

#[derive(Debug, Clone, Serialize, PartialEq, sqlx::FromRow)]
pub struct Brief {
    #[serde(skip)]
    pub rule_id: i64,
    pub observed_at: i64,
    pub status: ProbeStatus,
    pub latency_ms: Option<i64>,
    /// Samples and losses within the last hour.
    pub total: i64,
    pub lost: i64,
}

/// Latest sample and the last hour's loss for every listed rule of `owner`
/// (all owners when `None`), restricted to each rule's current target.
pub async fn briefs(
    pool: &SqlitePool,
    owner: Option<i64>,
    timestamp: i64,
) -> Result<Vec<Brief>, sqlx::Error> {
    // Rules drive the join so each reads only its last hour through the primary
    // key; the list polls every few seconds. SQLite takes bare columns of a MAX()
    // aggregate from the row holding the maximum: the latest sample's result.
    sqlx::query_as("SELECT r.id AS rule_id,MAX(s.observed_at) AS observed_at,s.status,s.latency_ms,COUNT(*) AS total,SUM(s.latency_ms IS NULL) AS lost FROM rules r CROSS JOIN rule_latency_samples s ON s.rule_id=r.id AND s.observed_at>? AND s.target_ip=r.target_ip AND s.target_port=r.target_port WHERE r.deleted_at IS NULL AND (? IS NULL OR r.owner_id=?) GROUP BY r.id")
        .bind(timestamp - 60 * 60).bind(owner).bind(owner).fetch_all(pool).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    async fn scripted(attempts: &[(&str, u64)]) -> (Probe, usize) {
        let queue = std::sync::Mutex::new(
            attempts
                .iter()
                .map(|(status, elapsed_ms)| TcpCheck {
                    status: (*status).into(),
                    elapsed_ms: *elapsed_ms,
                })
                .collect::<VecDeque<_>>(),
        );
        let probe = probe_with(|| {
            let next = queue
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected attempt");
            async move { next }
        })
        .await;
        let remaining = queue.lock().unwrap().len();
        (probe, attempts.len() - remaining)
    }

    #[tokio::test]
    async fn slow_handshakes_are_retried_and_lost_syns_count_as_loss() {
        let fast = scripted(&[("connected", 42)]).await;
        assert_eq!(fast, (reached(42), 1));
        let refused = scripted(&[("refused", 3)]).await.0;
        assert_eq!(refused.status, ProbeStatus::Refused);
        assert_eq!(refused.latency_ms, None);
        // The first SYN was lost: 1.04 s, then 40 ms on the retry.
        let retransmit = scripted(&[("connected", 1040), ("connected", 40)]).await;
        assert_eq!(retransmit.0.status, ProbeStatus::Retransmit);
        assert_eq!(retransmit.1, 2);
        // A genuinely slow path that improves slightly is still a latency.
        assert_eq!(
            scripted(&[("connected", 1200), ("connected", 900)]).await,
            (reached(900), 2)
        );
        let slow = scripted(&[
            ("connected", 1500),
            ("connected", 1300),
            ("connected", 1400),
            ("connected", 1350),
        ])
        .await;
        assert_eq!(slow, (reached(1300), 4));
        let lost = scripted(&[("connected", 1500), ("timeout", 2000)]).await.0;
        assert_eq!(lost.status, ProbeStatus::Timeout);
        assert!(lost.valid() && reached(42).valid());
        assert!(
            !Probe {
                status: ProbeStatus::Timeout,
                latency_ms: Some(1)
            }
            .valid()
        );
    }

    #[tokio::test]
    async fn batch_probes_keep_input_order_and_distinguish_refusals() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let open = listener.local_addr().unwrap();
        let closed = {
            let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            socket.local_addr().unwrap()
        };
        let probes = probe_all(&[closed, open, closed]).await;
        assert_eq!(probes.len(), 3);
        assert_eq!(probes[0].status, ProbeStatus::Refused);
        assert_eq!(probes[1].status, ProbeStatus::Connected);
        assert!(probes[1].latency_ms.is_some() && probes.iter().all(Probe::valid));
        assert_eq!(probes[2], probes[0]);
        let encoded = serde_json::to_value(probes[1]).unwrap();
        assert_eq!(encoded["status"], "connected");
    }

    #[test]
    fn summary_reports_loss_and_nearest_rank_percentiles() {
        let summary = summarize(&[Some(10), None, Some(30), Some(20), None]);
        assert_eq!(summary.total, 5);
        assert_eq!(summary.lost, 2);
        assert_eq!(summary.loss_percent, Some(40.0));
        assert_eq!(summary.avg_ms, Some(20));
        assert_eq!((summary.min_ms, summary.max_ms), (Some(10), Some(30)));
        assert_eq!((summary.p50_ms, summary.p99_ms), (Some(20), Some(30)));
        let empty = summarize(&[None]);
        assert_eq!(empty.loss_percent, Some(100.0));
        assert_eq!((empty.avg_ms, empty.p50_ms), (None, None));
        assert_eq!(summarize(&[]), Summary::default());
        let many: Vec<i64> = (1..=200).collect();
        assert_eq!(percentile(&many, 0.99), Some(198));
    }
}
