//! Host-only sampling. Client JSON never supplies a trusted clock or boot epoch.
use crate::rest_store::ClockSample;
use rusqlite::{Connection, OptionalExtension};

pub fn sample_in(db: &Connection) -> anyhow::Result<ClockSample> {
    Ok(sample_with_metadata_in(db)?.0)
}

fn sample_with_metadata_in(db: &Connection) -> anyhow::Result<(ClockSample, i64, bool)> {
    let wall = crate::state::now();
    let trusted: bool = db
        .query_row(
            "SELECT clock_trusted FROM trigger_runtime WHERE id=1",
            [],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(false);
    let (epoch, monotonic_ms) = monotonic_sample()?;
    Ok((
        ClockSample::new(wall, monotonic_ms, &epoch, trusted)?,
        wall,
        trusted,
    ))
}

/// Read-only host metadata; boot ID and the monotonic epoch stay server-side.
pub fn status_in(db: &Connection) -> serde_json::Value {
    match sample_with_metadata_in(db) {
        Ok((_, wall, trusted)) => {
            serde_json::json!({"host_sample_available":true,"trusted_for_new_sleep":trusted,"sampled_wall_at":wall,"client_clock_accepted":false})
        }
        Err(_) => {
            serde_json::json!({"host_sample_available":false,"trusted_for_new_sleep":false,"sampled_wall_at":null,"client_clock_accepted":false})
        }
    }
}

#[cfg(target_os = "linux")]
fn monotonic_sample() -> anyhow::Result<(String, i64)> {
    let epoch = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_owned();
    let uptime = std::fs::read_to_string("/proc/uptime")?;
    let first = uptime
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("monotonic uptime unavailable"))?;
    let (whole, fraction) = first.split_once('.').unwrap_or((first, ""));
    anyhow::ensure!(
        fraction.bytes().all(|v| v.is_ascii_digit()),
        "invalid host uptime"
    );
    let fractional = format!("{fraction:0<3}");
    let millis = whole
        .parse::<i64>()?
        .checked_mul(1000)
        .and_then(|v| {
            fractional[..3]
                .parse::<i64>()
                .ok()
                .and_then(|f| v.checked_add(f))
        })
        .ok_or_else(|| anyhow::anyhow!("monotonic uptime out of range"))?;
    Ok((epoch, millis))
}

// Development checks can use a process-scoped monotonic epoch. A new process
// gets a different epoch and restores a sleeping cycle by waking early, never
// by extending an unverifiable deadline. Formal Linux uses the kernel boot ID.
#[cfg(not(target_os = "linux"))]
fn monotonic_sample() -> anyhow::Result<(String, i64)> {
    use std::sync::OnceLock;
    use std::time::Instant;
    static SAMPLE: OnceLock<(String, Instant)> = OnceLock::new();
    let (epoch, start) = SAMPLE.get_or_init(|| {
        (
            format!("process-{}-{}", std::process::id(), uuid::Uuid::new_v4()),
            Instant::now(),
        )
    });
    Ok((
        epoch.clone(),
        start.elapsed().as_millis().min(i64::MAX as u128) as i64,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_is_fail_closed_and_tracks_host_trust_without_exposing_epoch() {
        let db = Connection::open_in_memory().unwrap();
        assert_eq!(status_in(&db)["trusted_for_new_sleep"], false);
        db.execute_batch("CREATE TABLE trigger_runtime(id INTEGER PRIMARY KEY,clock_trusted INTEGER); INSERT INTO trigger_runtime VALUES(1,0);").unwrap();
        let untrusted = status_in(&db);
        assert_eq!(untrusted["host_sample_available"], true);
        assert_eq!(untrusted["trusted_for_new_sleep"], false);
        db.execute("UPDATE trigger_runtime SET clock_trusted=1 WHERE id=1", [])
            .unwrap();
        let trusted = status_in(&db);
        assert_eq!(trusted["trusted_for_new_sleep"], true);
        assert!(trusted["sampled_wall_at"].as_i64().unwrap() > 0);
        assert_eq!(trusted["client_clock_accepted"], false);
        assert!(trusted.get("continuity_id").is_none());
    }
}
