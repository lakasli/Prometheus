use crate::{
    state::{now, AppState},
    trigger_store,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

pub struct ClockHealth {
    trusted: bool,
    source: String,
    reason: Option<String>,
    expires: Option<Instant>,
    sample: Instant,
    wall: i64,
}
impl Default for ClockHealth {
    fn default() -> Self {
        Self {
            trusted: false,
            source: "unverified".into(),
            reason: Some("尚未确认服务器时钟".into()),
            expires: None,
            sample: Instant::now(),
            wall: now(),
        }
    }
}
impl ClockHealth {
    pub fn status(&self) -> Value {
        let expired = self.expires.is_some_and(|i| i <= Instant::now());
        json!({"trusted":self.trusted&&!expired,"source":self.source,"reason":if expired{Some("人工时钟确认已过期")}else{self.reason.as_deref()},"expires_in_seconds":self.expires.map(|i|i.saturating_duration_since(Instant::now()).as_secs())})
    }
    fn set(&mut self, trusted: bool, source: &str, reason: Option<&str>, expires: Option<Instant>) {
        self.trusted = trusted;
        self.source = source.into();
        self.reason = reason.map(str::to_owned);
        self.expires = expires;
        self.sample = Instant::now();
        self.wall = now();
    }
    pub fn acknowledge(&mut self, observed: i64, confirmed: bool) -> anyhow::Result<()> {
        if !confirmed || observed.abs_diff(now()) > 10 {
            return Err(trigger_store::fail(
                "clock_confirmation_stale",
                "请核对当前显示的服务器时间后重新确认",
            ));
        };
        self.set(
            true,
            "administrator_confirmed",
            None,
            Some(Instant::now() + Duration::from_secs(3600)),
        );
        Ok(())
    }
    fn sample(&mut self) -> bool {
        let instant = Instant::now();
        let wall = now();
        let expected = self.wall + self.sample.elapsed().as_secs() as i64;
        let expired = self.expires.is_some_and(|i| i <= instant);
        if wall.abs_diff(expected) > 5 {
            self.set(
                false,
                "clock_jump",
                Some("服务器时钟发生跳变，请重新核对"),
                None,
            );
        } else if expired {
            self.set(
                false,
                "confirmation_expired",
                Some("人工时钟确认已到期，请重新核对或开启系统同步"),
                None,
            );
        }
        self.wall = wall;
        self.sample = instant;
        self.trusted
    }
}
pub fn clock_status(state: &AppState) -> Value {
    state
        .0
        .trigger_clock
        .lock()
        .map(|c| c.status())
        .unwrap_or_else(
            |_| json!({"trusted":false,"source":"unavailable","reason":"时钟状态不可用"}),
        )
}
pub fn acknowledge(
    state: &AppState,
    observed: i64,
    confirmed: bool,
) -> crate::error::ApiResult<Value> {
    let mut clock = state
        .0
        .trigger_clock
        .lock()
        .map_err(|_| crate::error::ApiError::internal())?;
    clock
        .acknowledge(observed, confirmed)
        .map_err(|e| crate::error::ApiError::bad("clock_confirmation_stale", e.to_string()))?;
    state.db(|db| {
        db.execute("UPDATE trigger_runtime SET clock_trusted=1 WHERE id=1", [])?;
        Ok(())
    })?;
    Ok(clock.status())
}
pub async fn run(state: AppState, cancel: CancellationToken) {
    let mut sync_check = Instant::now() - Duration::from_secs(61);
    let mut maintenance = Instant::now() - Duration::from_secs(61);
    let mut systems_maintenance = Instant::now() - Duration::from_secs(61);
    loop {
        if cancel.is_cancelled() {
            break;
        }
        #[cfg(target_os = "linux")]
        if sync_check.elapsed() >= Duration::from_secs(60) {
            sync_check = Instant::now();
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                tokio::process::Command::new("timedatectl")
                    .args(["show", "-p", "NTPSynchronized", "--value"])
                    .kill_on_drop(true)
                    .output(),
            )
            .await;
            let trusted = matches!(result,Ok(Ok(ref value)) if value.status.success()&&value.stdout.len()<64&&value.stdout.as_slice().trim_ascii()==b"yes");
            if let Ok(mut clock) = state.0.trigger_clock.lock() {
                if clock.source != "administrator_confirmed"
                    || clock.expires.is_none_or(|i| i <= Instant::now())
                {
                    if trusted {
                        clock.set(true, "os_ntp_synchronized", None, None);
                    } else {
                        clock.set(
                            false,
                            "os_sync_unavailable",
                            Some("系统时钟同步未确认，请人工核对"),
                            None,
                        );
                    }
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = &mut sync_check;
        if maintenance.elapsed() >= Duration::from_secs(60) {
            maintenance = Instant::now();
            let space_ok = fs2::available_space(&state.0.options.data_dir)
                .is_ok_and(|bytes| bytes >= 512 * 1024 * 1024);
            let _ = state.db(|db| {
                let tx = db.transaction()?;
                db_execute_space(&tx, space_ok)?;
                crate::trigger_retention::cleanup_in(&tx, now())?;
                tx.commit()?;
                Ok(())
            });
        }
        let trusted = state
            .0
            .trigger_clock
            .lock()
            .map(|mut c| c.sample())
            .unwrap_or(false);
        let result = state.db(|db| {
            db.execute(
                "UPDATE trigger_runtime SET clock_trusted=?1 WHERE id=1",
                [trusted],
            )?;
            Ok(())
        });
        if result.is_ok() {
            if trigger_store::tick(&state).is_err() {
                tracing::warn!("trigger scheduler tick unavailable");
            }
            state.0.notify.notify_waiters();
        }
        if trusted && systems_maintenance.elapsed() >= Duration::from_secs(30) {
            systems_maintenance = Instant::now();
            let ready = state.db(|db| {
                Ok(
                    db.query_row("SELECT space_ok FROM trigger_runtime WHERE id=1", [], |r| {
                        r.get::<_, bool>(0)
                    })?,
                )
            });
            if matches!(ready, Ok(true)) {
                // One persistent loop, independent short transactions. A full
                // diary period must not roll back unrelated scheduled work.
                if state
                    .db(|db| {
                        let tx = db.transaction()?;
                        crate::diary::tick_in(&tx, now())
                            .map_err(|_| anyhow::anyhow!("diary maintenance unavailable"))?;
                        tx.commit()?;
                        Ok(())
                    })
                    .is_err()
                {
                    tracing::warn!("diary maintenance unavailable");
                }
                if state.db(|db| {
                    let tx=db.transaction()?;
                    let busy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE run_kind IN ('model','skill_text') AND status IN ('queued','running'))",[],|r|r.get(0))?;
                    crate::schedule::tick_in(&tx,now(),busy)?;
                    tx.commit()?; Ok(())
                }).is_err() { tracing::warn!("schedule maintenance unavailable"); }
                if state
                    .db(|db| crate::relationships::maintenance_tick(db, now()))
                    .is_err()
                {
                    tracing::warn!("relationship maintenance unavailable");
                }
            }
            state.0.notify.notify_waiters();
        }
        tokio::select! {_=cancel.cancelled()=>break,_=tokio::time::sleep(Duration::from_millis(500))=>{},_=state.0.notify.notified()=>{tokio::time::sleep(Duration::from_millis(100)).await;}}
    }
}

fn db_execute_space(db: &rusqlite::Connection, ok: bool) -> anyhow::Result<()> {
    db.execute("UPDATE trigger_runtime SET space_ok=?1 WHERE id=1", [ok])?;
    if !ok {
        db.execute("UPDATE trigger_jobs SET block_reason='storage' WHERE lifecycle='enabled' AND block_reason IS NULL",[])?;
    } else {
        db.execute(
            "UPDATE trigger_jobs SET block_reason=NULL WHERE block_reason='storage'",
            [],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extreme_clock_confirmation_is_rejected_without_overflow() {
        let mut c = ClockHealth::default();
        assert!(c.acknowledge(i64::MIN, true).is_err());
        assert!(c.acknowledge(i64::MAX, true).is_err());
        assert!(c.acknowledge(now(), false).is_err());
        assert!(c.acknowledge(now(), true).is_ok());
        assert_eq!(c.status()["trusted"], true);
    }
    #[test]
    fn expired_confirmation_and_clock_jump_revoke_trust() {
        let mut c = ClockHealth::default();
        c.set(
            true,
            "administrator_confirmed",
            None,
            Some(Instant::now() - Duration::from_secs(1)),
        );
        assert!(!c.sample());
        c.set(true, "os_ntp_synchronized", None, None);
        c.wall -= 100;
        assert!(!c.sample());
        assert_eq!(c.status()["source"], "clock_jump");
    }
}
