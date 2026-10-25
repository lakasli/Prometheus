//! Bounded, exact-unit lifecycle for compiler and Component workers. A lease is
//! retained if exit cannot be proven, including cancellation during start.
use crate::{
    plugin_component_worker::{self, Operation, Request, Response},
    plugin_sandbox,
    state::{now, private_dir, private_write, AppState},
};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{Semaphore, SemaphorePermit},
};
use tokio_util::sync::CancellationToken;

static SLOTS: Semaphore = Semaphore::const_new(1);
struct Lease {
    state: AppState,
    permit: Option<SemaphorePermit<'static>>,
    started: bool,
    confirmed: bool,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if self.started && !self.confirmed {
            if let Some(permit) = self.permit.take() {
                permit.forget();
            }
            // A dropped/aborted handler can kill the launcher while its
            // systemd unit still starts or runs. Keep the persisted unit and
            // block every new worker until a fresh daemon proves exact exit.
            let _ = self.state.db(|db| {
                db.execute(
                    "UPDATE plugin_factory_runtime SET verified=0,checked_at=?1 WHERE id=1",
                    [now()],
                )?;
                Ok(())
            });
        }
    }
}
#[derive(Debug)]
pub struct Failure {
    pub code: &'static str,
    pub exit_confirmed: bool,
}
fn error(code: &'static str) -> Failure {
    Failure {
        code,
        exit_confirmed: true,
    }
}
fn manager(program: &str) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("LANG", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for key in ["XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
}
async fn properties(unit: &str) -> Result<String, Failure> {
    let mut command = manager("/usr/bin/systemctl");
    command.args([
        "--user",
        "show",
        "--no-pager",
        "--property=LoadState,ActiveState,ControlGroup,MemoryMax,MemorySwapMax,TasksMax",
        "--",
        unit,
    ]);
    let result = tokio::time::timeout(Duration::from_secs(3), command.output())
        .await
        .map_err(|_| error("plugin_manager_timeout"))?
        .map_err(|_| error("plugin_manager_unavailable"))?;
    if result.stdout.len() > 16384 {
        return Err(error("plugin_manager_protocol"));
    }
    String::from_utf8(result.stdout).map_err(|_| error("plugin_manager_protocol"))
}
fn prop<'a>(properties: &'a str, key: &str) -> Option<&'a str> {
    properties
        .lines()
        .find_map(|s| s.strip_prefix(key).and_then(|s| s.strip_prefix('=')))
}
pub fn valid_unit(unit: &str) -> bool {
    unit.strip_prefix("dsh-plugin-")
        .and_then(|s| s.strip_suffix(".service"))
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .is_some_and(|v| format!("dsh-plugin-{v}.service") == unit && v.get_version_num() == 4)
}
async fn stop(unit: &str) -> Result<(), Failure> {
    if !valid_unit(unit) {
        return Err(Failure {
            code: "plugin_unit_invalid",
            exit_confirmed: false,
        });
    }
    let mut command = manager("/usr/bin/systemctl");
    command.args(["--user", "stop", "--", unit]);
    let _ = tokio::time::timeout(Duration::from_secs(5), command.output()).await;
    let values = properties(unit).await.map_err(|_| Failure {
        code: "plugin_exit_unknown",
        exit_confirmed: false,
    })?;
    if prop(&values, "LoadState") == Some("not-found")
        || matches!(prop(&values, "ActiveState"), Some("inactive" | "failed"))
    {
        // An inactive unit with a lingering cgroup process is not sufficient.
        if let Some(group) = prop(&values, "ControlGroup").filter(|g| !g.is_empty()) {
            if !group.starts_with('/')
                || group.contains("..")
                || group.rsplit('/').next() != Some(unit)
            {
                return Err(Failure {
                    code: "plugin_exit_unknown",
                    exit_confirmed: false,
                });
            }
            if let Ok(events) = std::fs::read_to_string(
                Path::new("/sys/fs/cgroup")
                    .join(group.trim_start_matches('/'))
                    .join("cgroup.events"),
            ) {
                if events.lines().any(|line| line == "populated 1") {
                    return Err(Failure {
                        code: "plugin_exit_unknown",
                        exit_confirmed: false,
                    });
                }
            }
        }
        Ok(())
    } else {
        Err(Failure {
            code: "plugin_exit_unknown",
            exit_confirmed: false,
        })
    }
}
fn verify_cgroup(unit: &str, values: &str) -> bool {
    let Some(group) = prop(values, "ControlGroup") else {
        return false;
    };
    if !group.starts_with('/') || group.contains("..") || group.rsplit('/').next() != Some(unit) {
        return false;
    }
    let root = Path::new("/sys/fs/cgroup").join(group.trim_start_matches('/'));
    let number = |name| {
        std::fs::read_to_string(root.join(name))
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
    };
    number("memory.max") == Some(plugin_sandbox::MEMORY_BYTES)
        && number("memory.swap.max") == Some(0)
        && number("pids.max") == Some(plugin_sandbox::TASKS)
}
fn cleanup(root: &Path, expected_parent: &Path) {
    // Only the unique directory created by this invocation, after exact exit.
    if root.parent() != Some(expected_parent)
        || root
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .is_none()
    {
        return;
    }
    if std::fs::symlink_metadata(root).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
        && root.canonicalize().is_ok_and(|p| p == root)
    {
        let _ = std::fs::remove_dir_all(root);
    }
}
pub async fn initialize_runtime(state: &AppState) {
    let unfinished = state.db(|db| {
        let mut stmt =
            db.prepare("SELECT unit_name FROM plugin_factory_units WHERE finished_at IS NULL")?;
        let units = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(units)
    });
    if let Ok(units) = unfinished {
        for unit in units {
            if stop(&unit).await.is_ok() {
                let _ = state.db(|db| {
                    db.execute(
                        "UPDATE plugin_factory_units SET finished_at=?1 WHERE unit_name=?2",
                        rusqlite::params![now(), unit],
                    )?;
                    Ok(())
                });
            } else {
                if let Ok(permit) = SLOTS.try_acquire() {
                    permit.forget();
                }
                tracing::warn!("plugin worker exit remains unverified");
                break;
            }
        }
    }
    if plugin_sandbox::available() {
        let _ = verify_runtime(state).await;
    }
}
pub async fn verify_runtime(state: &AppState) -> Result<Response, Failure> {
    let response = execute(
        state,
        &uuid::Uuid::new_v4().to_string(),
        Operation::Probe,
        &CancellationToken::new(),
    )
    .await;
    if response
        .as_ref()
        .is_err_and(|failure| failure.code == "plugin_worker_busy")
    {
        // Contention with a healthy in-flight worker is not a failed isolation
        // proof. An unknown-exit Lease has already closed the gate itself.
        return response;
    }
    let verified = response.as_ref().is_ok_and(|value| value.ok);
    state
        .db(|db| {
            db.execute(
                "UPDATE plugin_factory_runtime SET verified=?1,checked_at=?2 WHERE id=1",
                rusqlite::params![verified, now()],
            )?;
            Ok(())
        })
        .map_err(|_| error("plugin_ledger_unavailable"))?;
    response
}
pub async fn execute(
    state: &AppState,
    job_id: &str,
    operation: Operation,
    cancel: &CancellationToken,
) -> Result<Response, Failure> {
    if !cfg!(target_os = "linux") {
        return Err(error("plugin_linux_required"));
    }
    let permit = SLOTS
        .try_acquire()
        .map_err(|_| error("plugin_worker_busy"))?;
    let rustc = plugin_sandbox::toolchain().map_err(error)?;
    let binary = std::env::current_exe()
        .map_err(|_| error("plugin_worker_unavailable"))?
        .canonicalize()
        .map_err(|_| error("plugin_worker_unavailable"))?;
    let parent = state
        .0
        .options
        .data_dir
        .canonicalize()
        .map_err(|_| error("plugin_workspace_unavailable"))?
        .join("plugin-factory-workers");
    private_dir(&parent).map_err(|_| error("plugin_workspace_unavailable"))?;
    let root = parent.join(uuid::Uuid::new_v4().to_string());
    private_dir(&root).map_err(|_| error("plugin_workspace_unavailable"))?;
    let work = root.join("work");
    let hidden = root.join("hidden");
    for dir in [&work, &hidden] {
        private_dir(dir).map_err(|_| error("plugin_workspace_unavailable"))?;
    }
    let canary = hidden.join("synthetic-private-key");
    private_write(&canary, uuid::Uuid::new_v4().to_string().as_bytes())
        .map_err(|_| error("plugin_workspace_unavailable"))?;
    // Probe an explicitly created synthetic peer, never the daemon's real
    // environment, descriptors, production secrets or another user's process.
    let mut peer = Command::new("/usr/bin/sleep")
        .arg("110")
        .env_clear()
        .env("DSH_SYNTHETIC_CANARY", uuid::Uuid::new_v4().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| error("plugin_probe_peer_unavailable"))?;
    let peer_pid = peer
        .id()
        .ok_or_else(|| error("plugin_probe_peer_unavailable"))?;
    let request = Request {
        operation,
        forbidden_canary: canary.to_string_lossy().into_owned(),
        forbidden_directory: hidden.to_string_lossy().into_owned(),
        peer_pid,
    };
    let bytes = serde_json::to_vec(&request).map_err(|_| error("plugin_protocol_invalid"))?;
    if bytes.len() > plugin_component_worker::MAX_PROTOCOL {
        cleanup(&root, &parent);
        return Err(error("plugin_protocol_too_large"));
    }
    let unit = format!("dsh-plugin-{}.service", uuid::Uuid::new_v4());
    let mut lease = Lease {
        state: state.clone(),
        permit: Some(permit),
        started: false,
        confirmed: false,
    };
    state
        .db(|db| {
            db.execute(
                "INSERT INTO plugin_factory_units(unit_name,job_id,started_at) VALUES(?1,?2,?3)",
                rusqlite::params![unit, job_id, now()],
            )?;
            Ok(())
        })
        .map_err(|_| error("plugin_ledger_unavailable"))?;
    let mut command = manager("/usr/bin/systemd-run");
    command
        .args([
            "--user",
            "--wait",
            "--pipe",
            "--quiet",
            "--unit",
            &unit,
            "--property=Type=exec",
            "--property=MemoryMax=1G",
            "--property=MemorySwapMax=0",
            "--property=TasksMax=32",
            "--property=RuntimeMaxSec=90s",
            "--property=TimeoutStopSec=2s",
            "--property=KillMode=control-group",
            "--property=NoNewPrivileges=yes",
            "--property=OOMPolicy=kill",
            "--property=LimitFSIZE=16777216",
            "--property=LimitCORE=0",
            "--property=LimitCPU=60",
            "--property=LimitNOFILE=64",
            "--",
            "/usr/bin/env",
            "-i",
            "LANG=C",
        ])
        .arg(&binary)
        .args(["plugin-component-worker", "--work-dir"])
        .arg(&work)
        .arg("--rustc")
        .arg(&rustc)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    lease.started = true;
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            lease.confirmed = true;
            cleanup(&root, &parent);
            let _ = state.db(|db| {
                db.execute(
                    "UPDATE plugin_factory_units SET finished_at=?1 WHERE unit_name=?2",
                    rusqlite::params![now(), unit],
                )?;
                Ok(())
            });
            return Err(error("plugin_manager_unavailable"));
        }
    };
    let mut observed = false;
    let operation = async {
        let mut verified = false;
        for _ in 0..40 {
            if let Ok(values) = properties(&unit).await {
                observed |= prop(&values, "LoadState") == Some("loaded");
                if verify_cgroup(&unit, &values) {
                    verified = true;
                    break;
                }
                // StartTransientUnit can expose a loaded but initially
                // inactive unit before its start job creates the cgroup. That
                // is not an exited worker. Keep polling; request bytes are
                // withheld until the kernel cgroup values are verified.
                if observed && prop(&values, "ActiveState") == Some("failed") {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if !verified {
            return Err(error("plugin_cgroup_unverified"));
        }
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| error("plugin_protocol_invalid"))?;
        stdin
            .write_all(&bytes)
            .await
            .map_err(|_| error("plugin_protocol_invalid"))?;
        stdin
            .shutdown()
            .await
            .map_err(|_| error("plugin_protocol_invalid"))?;
        drop(stdin);
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| error("plugin_protocol_invalid"))?;
        let mut output = Vec::new();
        stdout
            .take((plugin_component_worker::MAX_PROTOCOL + 1) as u64)
            .read_to_end(&mut output)
            .await
            .map_err(|_| error("plugin_protocol_invalid"))?;
        if output.len() > plugin_component_worker::MAX_PROTOCOL {
            return Err(error("plugin_protocol_too_large"));
        }
        let status = child
            .wait()
            .await
            .map_err(|_| error("plugin_worker_exit"))?;
        let response: Response = serde_json::from_slice(&output).map_err(|_| {
            let value = serde_json::from_slice::<serde_json::Value>(&output).ok();
            error(
                match value
                    .as_ref()
                    .and_then(|value| value["preflight_error"].as_str())
                {
                    Some("plugin_landlock_rule_rejected") => "plugin_landlock_rule_rejected",
                    Some("plugin_landlock_enforcement_failed") => {
                        "plugin_landlock_enforcement_failed"
                    }
                    Some("plugin_seccomp_unavailable") => "plugin_seccomp_unavailable",
                    Some("plugin_cgroup_unverified") => "plugin_cgroup_unverified",
                    Some("plugin_rlimits_unverified") => "plugin_rlimits_unverified",
                    Some("plugin_pipe_ioctls_unverified") => "plugin_pipe_ioctls_unverified",
                    Some("plugin_clone_flags_unverified") => "plugin_clone_flags_unverified",
                    Some("plugin_sandbox_path_unavailable") => "plugin_sandbox_path_unavailable",
                    Some("plugin_close_range_unavailable") => "plugin_close_range_unavailable",
                    _ => "plugin_worker_preflight_failed",
                },
            )
        })?;
        if response.engine != plugin_component_worker::ENGINE
            || response.host_api != plugin_component_worker::HOST_API
            || response.isolation.policy != plugin_sandbox::POLICY
            || response.isolation.unit != unit
            || response.isolation.landlock_abi < 4
            || !response.isolation.seccomp
            || !response.isolation.no_new_privileges
            || !response.isolation.inherited_fds_closed
            || !response.isolation.rlimits_enforced
            || !response.isolation.pipe_ioctls_verified
            || !response.isolation.clone_flags_verified
            || response.isolation.cgroup_memory_bytes != plugin_sandbox::MEMORY_BYTES
            || response.isolation.cgroup_tasks != plugin_sandbox::TASKS
            || response.ok && !plugin_component_worker::probes_passed(&response.probes)
        {
            return Err(error("plugin_isolation_unverified"));
        }
        if status.success() != response.ok {
            return Err(error("plugin_protocol_invalid"));
        }
        Ok(response)
    };
    let result = tokio::select! {biased;_=cancel.cancelled()=>Err(error("plugin_cancelled")),_=tokio::time::sleep(Duration::from_secs(100))=>Err(error("plugin_worker_timeout")),result=operation=>result};
    let launcher_finished = child.try_wait().ok().flatten().is_some();
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
    let confirmed = stop(&unit).await.is_ok() && (observed || launcher_finished);
    let _ = peer.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(3), peer.wait()).await;
    if !confirmed {
        return Err(Failure {
            code: "plugin_exit_unknown",
            exit_confirmed: false,
        });
    }
    lease.confirmed = true;
    state
        .db(|db| {
            db.execute(
                "UPDATE plugin_factory_units SET finished_at=?1 WHERE unit_name=?2",
                rusqlite::params![now(), unit],
            )?;
            Ok(())
        })
        .map_err(|_| error("plugin_ledger_unavailable"))?;
    cleanup(&root, &parent);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn abort_keeps_an_unconfirmed_worker_slot_and_closes_the_runtime_gate() {
        let directory = tempfile::tempdir().unwrap();
        let state = AppState::open(crate::state::Options {
            data_dir: directory.path().join("data"),
            origin: "http://127.0.0.1:18419".into(),
            allow_private_model: false,
            cookie_secure: false,
            web_dir: directory.path().to_owned(),
        })
        .unwrap();
        state
            .db(|db| {
                db.execute(
                    "UPDATE plugin_factory_runtime SET verified=1 WHERE id=1",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let slots: &'static Semaphore = Box::leak(Box::new(Semaphore::new(1)));
        let started = std::sync::Arc::new(tokio::sync::Notify::new());
        let signal = started.clone();
        let task_state = state.clone();
        let task = tokio::spawn(async move {
            let _lease = Lease {
                state: task_state,
                permit: Some(slots.acquire().await.unwrap()),
                started: true,
                confirmed: false,
            };
            signal.notify_one();
            std::future::pending::<()>().await;
        });
        started.notified().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(
            slots.available_permits(),
            0,
            "a launcher drop cannot release a still-live systemd worker"
        );
        assert!(!state
            .db(|db| Ok(db.query_row(
                "SELECT verified=1 FROM plugin_factory_runtime WHERE id=1",
                [],
                |r| r.get::<_, bool>(0)
            )?))
            .unwrap());
        let confirmed_slots: &'static Semaphore = Box::leak(Box::new(Semaphore::new(1)));
        let lease = Lease {
            state,
            permit: Some(confirmed_slots.acquire().await.unwrap()),
            started: true,
            confirmed: true,
        };
        drop(lease);
        assert_eq!(
            confirmed_slots.available_permits(),
            1,
            "only confirmed exact exit releases capacity"
        );
    }
}
