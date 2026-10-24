//! Exact-unit supervision. The parent only checks sizes, protocol and hashes;
//! untrusted parsing and execution are confined to a bounded systemd worker.
use crate::wasm_worker::{self, Case, Outcome, Request, ResourceInfo, ResourceProbeKind, Response};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::{collections::HashMap, path::Path, process::Stdio, sync::Mutex, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::{Semaphore, SemaphorePermit},
};
use tokio_util::sync::CancellationToken;

static SLOTS: Semaphore = Semaphore::const_new(1);
static HELD: Mutex<Option<String>> = Mutex::new(None);
#[derive(Clone, Copy, Debug)]
pub struct SupervisorError {
    pub code: &'static str,
    pub exit_confirmed: bool,
}
impl std::fmt::Display for SupervisorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code)
    }
}
impl std::error::Error for SupervisorError {}
fn error(code: &'static str) -> SupervisorError {
    SupervisorError {
        code,
        exit_confirmed: true,
    }
}
#[derive(Clone, Debug)]
pub struct CompileResult {
    pub wasm: Vec<u8>,
    pub wasm_sha256: String,
    pub wat_sha256: String,
    pub results: Vec<i32>,
    pub engine: String,
    pub policy: String,
    pub resources: ResourceInfo,
}
#[derive(Clone, Debug)]
pub struct ExecutionResult {
    pub result: i32,
    pub engine: String,
    pub policy: String,
    pub resources: ResourceInfo,
}
#[derive(Clone, Debug)]
pub struct ProbeInfo {
    pub engine: String,
    pub policy: String,
    pub resources: ResourceInfo,
    pub runtime_max_usec: u64,
}
#[derive(Clone, Debug)]
pub struct ResourceProbeResult {
    pub kind: ResourceProbeKind,
    pub passed: bool,
    pub unit_result: String,
    pub resources: ResourceInfo,
    pub runtime_max_usec: u64,
}
pub fn new_unit_name() -> String {
    format!("dsh-wasm-{}.service", uuid::Uuid::new_v4())
}

struct Lease {
    unit: String,
    permit: Option<SemaphorePermit<'static>>,
    confirmed: bool,
}
impl Lease {
    fn acquire(unit: &str) -> Result<Self, SupervisorError> {
        if !wasm_worker::valid_unit_name(unit) {
            return Err(error("invalid_worker_unit"));
        }
        let permit = SLOTS.try_acquire().map_err(|_| error("wasm_worker_busy"))?;
        Ok(Self {
            unit: unit.into(),
            permit: Some(permit),
            confirmed: false,
        })
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if !self.confirmed {
            if let Some(permit) = self.permit.take() {
                permit.forget();
                if let Ok(mut held) = HELD.lock() {
                    *held = Some(self.unit.clone());
                }
            }
        }
    }
}
fn manager_command(program: &str) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    // These are trusted local bus coordinates, never model credentials. The
    // actual ExecStart below uses env -i, so they do not reach the worker.
    for key in ["XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command.env("LANG", "C");
    command
}
async fn output(mut command: Command) -> Result<(bool, String), SupervisorError> {
    let result = tokio::time::timeout(Duration::from_secs(2), command.output())
        .await
        .map_err(|_| error("worker_manager_timeout"))?
        .map_err(|_| error("worker_manager_unavailable"))?;
    if result.stdout.len() > 16384 {
        return Err(error("worker_manager_protocol"));
    }
    let text = String::from_utf8(result.stdout).map_err(|_| error("worker_manager_protocol"))?;
    Ok((result.status.success(), text))
}
type Properties = HashMap<String, String>;
fn properties(text: &str) -> Properties {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
}
async fn show(unit: &str) -> Result<Properties, SupervisorError> {
    let mut command = manager_command("/usr/bin/systemctl");
    command.args(["--user","show","--no-pager","--property=LoadState,ActiveState,SubState,Type,ControlGroup,MemoryMax,MemorySwapMax,TasksMax,RuntimeMaxUSec,TimeoutStopUSec,Result","--",unit]);
    let (_, text) = output(command).await?;
    let result = properties(&text);
    if !result.contains_key("LoadState") {
        return Err(error("worker_manager_protocol"));
    }
    Ok(result)
}
fn duration_usec(value: &str) -> Option<u64> {
    let value = value.trim();
    if let Some(value) = value.strip_suffix("us") {
        return value.trim().parse().ok();
    }
    if let Some(value) = value.strip_suffix("ms") {
        return value.trim().parse::<u64>().ok()?.checked_mul(1000);
    }
    if let Some(value) = value.strip_suffix('s') {
        return value.trim().parse::<u64>().ok()?.checked_mul(1_000_000);
    }
    value.parse().ok()
}
fn cgroup_path<'a>(unit: &str, group: &'a str) -> Result<&'a Path, SupervisorError> {
    let path = Path::new(group);
    if !group.starts_with('/')
        || group.contains('\\')
        || group
            .split('/')
            .skip(1)
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
        || group.rsplit('/').next() != Some(unit)
    {
        return Err(error("worker_cgroup_unverified"));
    }
    Ok(path)
}
fn kernel_limits_at(root: &Path, unit: &str, group: &str) -> Result<ResourceInfo, SupervisorError> {
    cgroup_path(unit, group)?;
    let directory = root.join(group.trim_start_matches('/'));
    let number = |name: &str| -> Result<u64, SupervisorError> {
        std::fs::read_to_string(directory.join(name))
            .map_err(|_| error("worker_cgroup_unverified"))?
            .trim()
            .parse()
            .map_err(|_| error("worker_cgroup_unverified"))
    };
    let memory = number("memory.max")?;
    let swap = number("memory.swap.max")?;
    let pids = number("pids.max")?;
    if memory != wasm_worker::MEMORY_MAX_BYTES || swap != 0 || pids != 1 {
        return Err(error("worker_cgroup_unverified"));
    }
    Ok(ResourceInfo {
        enforced: true,
        memory_max_bytes: Some(memory),
        swap_max_bytes: Some(swap),
        pids_max: Some(pids),
        unit: Some(unit.into()),
    })
}
fn verify_at(root: &Path, unit: &str, props: &Properties) -> Result<ResourceInfo, SupervisorError> {
    let get = |name: &str| props.get(name).map(String::as_str).unwrap_or("");
    if get("LoadState") != "loaded"
        || get("ActiveState") != "active"
        || get("SubState") != "running"
        || get("Type") != "exec"
        || get("MemoryMax") != "134217728"
        || get("MemorySwapMax") != "0"
        || get("TasksMax") != "1"
        || duration_usec(get("RuntimeMaxUSec")) != Some(5_000_000)
        || duration_usec(get("TimeoutStopUSec")) != Some(1_000_000)
    {
        return Err(error("worker_enforcement_unverified"));
    }
    kernel_limits_at(root, unit, get("ControlGroup"))
}
fn empty_group(props: &Properties, unit: &str) -> Result<bool, SupervisorError> {
    let group = props.get("ControlGroup").map(String::as_str).unwrap_or("");
    if group.is_empty() {
        return Ok(true);
    }
    cgroup_path(unit, group)?;
    let path = Path::new("/sys/fs/cgroup")
        .join(group.trim_start_matches('/'))
        .join("cgroup.events");
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text.lines().any(|line| line == "populated 0")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(_) => Err(error("worker_exit_unknown")),
    }
}
async fn confirm_inactive(unit: &str) -> Result<(), SupervisorError> {
    for _ in 0..15 {
        let props = show(unit).await?;
        let state = props.get("ActiveState").map(String::as_str).unwrap_or("");
        let missing = props.get("LoadState").is_some_and(|s| s == "not-found");
        if (missing || matches!(state, "inactive" | "failed")) && empty_group(&props, unit)? {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(SupervisorError {
        code: "worker_exit_unknown",
        exit_confirmed: false,
    })
}
async fn stop(unit: &str) -> Result<(), SupervisorError> {
    let mut command = manager_command("/usr/bin/systemctl");
    command.args(["--user", "stop", "--", unit]);
    let _ = output(command).await;
    confirm_inactive(unit).await.map_err(|_| SupervisorError {
        code: "worker_exit_unknown",
        exit_confirmed: false,
    })?;
    let mut reset = manager_command("/usr/bin/systemctl");
    reset.args(["--user", "reset-failed", "--", unit]);
    let _ = output(reset).await;
    Ok(())
}
/// Only a caller's previously recorded exact unit may be supplied. Never lists
/// or stops units belonging to another daemon/data directory.
pub async fn stop_orphan(unit: &str) -> Result<(), SupervisorError> {
    if !wasm_worker::valid_unit_name(unit) {
        return Err(error("invalid_worker_unit"));
    }
    if !cfg!(target_os = "linux") {
        return Err(error("worker_platform_unavailable"));
    }
    stop(unit).await?;
    let mut held = HELD.lock().map_err(|_| SupervisorError {
        code: "worker_exit_unknown",
        exit_confirmed: false,
    })?;
    if held.as_deref() == Some(unit) {
        *held = None;
        SLOTS.add_permits(1);
    }
    Ok(())
}
struct Transport {
    response: Option<Response>,
    resources: ResourceInfo,
    unit_result: String,
}
async fn read_child(child: &mut Child) -> Result<(bool, Vec<u8>), SupervisorError> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| error("worker_protocol_error"))?;
    let mut bytes = Vec::new();
    stdout
        .take((wasm_worker::MAX_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| error("worker_protocol_error"))?;
    if bytes.len() > wasm_worker::MAX_RESPONSE_BYTES {
        return Err(error("worker_protocol_error"));
    }
    let status = child
        .wait()
        .await
        .map_err(|_| error("worker_exit_unknown"))?;
    Ok((status.success(), bytes))
}
async fn supervised(
    unit: &str,
    request: Request,
    cancel: &CancellationToken,
) -> Result<Transport, SupervisorError> {
    if !cfg!(target_os = "linux") {
        return Err(error("worker_platform_unavailable"));
    }
    let bytes = serde_json::to_vec(&request).map_err(|_| error("worker_protocol_error"))?;
    if bytes.len() > wasm_worker::MAX_REQUEST_BYTES {
        return Err(error("code_too_large"));
    }
    if cancel.is_cancelled() {
        return Err(error("wasm_cancelled"));
    }
    let binary = std::env::current_exe().map_err(|_| error("worker_binary_unavailable"))?;
    let mut lease = Lease::acquire(unit)?;
    let mut command = manager_command("/usr/bin/systemd-run");
    command
        .args([
            "--user",
            "--wait",
            "--pipe",
            "--quiet",
            "--unit",
            unit,
            "--property=Type=exec",
            "--property=MemoryMax=128M",
            "--property=MemorySwapMax=0",
            "--property=TasksMax=1",
            "--property=RuntimeMaxSec=5s",
            "--property=TimeoutStopSec=1s",
            "--property=KillMode=control-group",
            "--property=NoNewPrivileges=yes",
            "--property=OOMPolicy=kill",
            "--",
            "/usr/bin/env",
            "-i",
            "LANG=C",
        ])
        .arg(binary)
        .args(["wasm-worker", "--require-cgroup"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            lease.confirmed = true;
            return Err(error("worker_manager_unavailable"));
        }
    };
    let mut observed_unit = false;
    let operation = async {
        let mut verified = None;
        for _ in 0..20 {
            if let Ok(props) = show(unit).await {
                let loaded = props.get("LoadState").is_some_and(|s| s == "loaded");
                observed_unit |= loaded;
                if let Ok(resources) = verify_at(Path::new("/sys/fs/cgroup"), unit, &props) {
                    verified = Some(resources);
                    break;
                }
                if loaded
                    && props
                        .get("ActiveState")
                        .is_some_and(|s| matches!(s.as_str(), "failed" | "inactive"))
                {
                    return Err(error("worker_enforcement_unverified"));
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let resources = verified.ok_or_else(|| error("worker_enforcement_unverified"))?;
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| error("worker_protocol_error"))?;
        input
            .write_all(&bytes)
            .await
            .map_err(|_| error("worker_protocol_error"))?;
        input
            .shutdown()
            .await
            .map_err(|_| error("worker_protocol_error"))?;
        drop(input);
        let (success, output) = read_child(&mut child).await?;
        let response = if output.is_empty() {
            None
        } else {
            Some(
                serde_json::from_slice::<Response>(&output)
                    .map_err(|_| error("worker_protocol_error"))?,
            )
        };
        if success && !matches!(response, Some(Response::Ok { .. })) {
            return Err(error("worker_protocol_error"));
        }
        if let Some(Response::Ok {
            ref engine,
            ref policy,
            resources: ref returned,
            ..
        }) = response
        {
            if engine != wasm_worker::ENGINE
                || policy != wasm_worker::POLICY
                || returned != &resources
            {
                return Err(error("worker_protocol_error"));
            }
        }
        Ok((response, resources))
    };
    let outcome = tokio::select! {biased;_=cancel.cancelled()=>Err(error("wasm_cancelled")),_=tokio::time::sleep(Duration::from_secs(8))=>Err(error("worker_deadline")),outcome=operation=>outcome};
    let final_properties = show(unit).await.ok();
    observed_unit |= final_properties
        .as_ref()
        .is_some_and(|p| p.get("LoadState").is_some_and(|s| s == "loaded"));
    let unit_result = final_properties
        .and_then(|p| p.get("Result").cloned())
        .unwrap_or_default();
    // Stop the launcher before the final unit check. Otherwise a cancelled
    // in-flight StartTransientUnit could create a service after a not-found
    // observation and prematurely release its scheduling lease.
    let launcher_finished = child.try_wait().ok().flatten().is_some();
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
    stop(unit).await?;
    if !observed_unit && !launcher_finished {
        return Err(SupervisorError {
            code: "worker_start_unknown",
            exit_confirmed: false,
        });
    }
    lease.confirmed = true;
    let (response, resources) = outcome?;
    Ok(Transport {
        response,
        resources,
        unit_result,
    })
}
fn response_error(response: Option<Response>) -> SupervisorError {
    match response {
        Some(Response::Error { code }) => error(code.as_str()),
        _ => error("worker_failed"),
    }
}
pub async fn probe(unit: &str, cancel: &CancellationToken) -> Result<ProbeInfo, SupervisorError> {
    let transport = supervised(unit, Request::Probe, cancel).await?;
    match transport.response {
        Some(Response::Ok {
            engine,
            policy,
            outcome,
            ..
        }) if matches!(*outcome, Outcome::Probe) => Ok(ProbeInfo {
            engine,
            policy,
            resources: transport.resources,
            runtime_max_usec: 5_000_000,
        }),
        response => Err(response_error(response)),
    }
}
pub async fn compile_and_test(
    unit: &str,
    wat: String,
    cases: Vec<Case>,
    cancel: &CancellationToken,
) -> Result<CompileResult, SupervisorError> {
    if wat.is_empty() || wat.len() > wasm_worker::MAX_CODE_BYTES || !(2..=8).contains(&cases.len())
    {
        return Err(error("invalid_wasm_request"));
    }
    let source_hash = wasm_worker::sha256(wat.as_bytes());
    let expected: Vec<_> = cases.iter().map(|c| c.expected).collect();
    let transport = supervised(unit, Request::CompileAndTest { wat, cases }, cancel).await?;
    match transport.response {
        Some(Response::Ok {
            engine,
            policy,
            outcome,
            ..
        }) => {
            let Outcome::CompileAndTest {
                wasm_base64,
                wasm_sha256,
                wat_sha256,
                results,
            } = *outcome
            else {
                return Err(error("worker_protocol_error"));
            };
            if wasm_base64.len() > wasm_worker::MAX_CODE_BYTES.div_ceil(3) * 4 {
                return Err(error("worker_protocol_error"));
            }
            let wasm = STANDARD
                .decode(wasm_base64)
                .map_err(|_| error("worker_protocol_error"))?;
            if wasm.is_empty()
                || wasm.len() > wasm_worker::MAX_CODE_BYTES
                || wasm_worker::sha256(&wasm) != wasm_sha256
                || source_hash != wat_sha256
                || results != expected
            {
                return Err(error("worker_protocol_error"));
            }
            Ok(CompileResult {
                wasm,
                wasm_sha256,
                wat_sha256,
                results,
                engine,
                policy,
                resources: transport.resources,
            })
        }
        response => Err(response_error(response)),
    }
}
pub async fn execute(
    unit: &str,
    wasm: Vec<u8>,
    args: [i32; 2],
    cancel: &CancellationToken,
) -> Result<ExecutionResult, SupervisorError> {
    if wasm.is_empty() || wasm.len() > wasm_worker::MAX_CODE_BYTES {
        return Err(error("invalid_wasm_request"));
    }
    let transport = supervised(
        unit,
        Request::Execute {
            wasm_base64: STANDARD.encode(wasm),
            args,
        },
        cancel,
    )
    .await?;
    match transport.response {
        Some(Response::Ok {
            engine,
            policy,
            outcome,
            ..
        }) => {
            let Outcome::Execute { result } = *outcome else {
                return Err(error("worker_protocol_error"));
            };
            Ok(ExecutionResult {
                result,
                engine,
                policy,
                resources: transport.resources,
            })
        }
        response => Err(response_error(response)),
    }
}
/// Explicit operator-only pressure verification. Never run during ordinary startup.
pub async fn resource_probe(
    unit: &str,
    kind: ResourceProbeKind,
    cancel: &CancellationToken,
) -> Result<ResourceProbeResult, SupervisorError> {
    let transport = supervised(unit, Request::ResourceProbe { kind }, cancel).await?;
    let expected = match kind {
        ResourceProbeKind::Memory => "oom-kill",
        ResourceProbeKind::Timeout => "timeout",
    };
    Ok(ResourceProbeResult {
        kind,
        passed: transport.response.is_none() && transport.unit_result == expected,
        unit_result: transport.unit_result,
        resources: transport.resources,
        runtime_max_usec: 5_000_000,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_systemd_255_time_formats_and_kernel_limits_are_required_together() {
        let root = tempfile::tempdir().unwrap();
        let unit = new_unit_name();
        let group = format!("/user.slice/user-1000.slice/user@1000.service/app.slice/{unit}");
        let directory = root.path().join(group.trim_start_matches('/'));
        std::fs::create_dir_all(&directory).unwrap();
        for (name, value) in [
            ("memory.max", "134217728"),
            ("memory.swap.max", "0"),
            ("pids.max", "1"),
        ] {
            std::fs::write(directory.join(name), value).unwrap();
        }
        let mut props=properties(&format!("LoadState=loaded\nActiveState=active\nSubState=running\nType=exec\nControlGroup={group}\nMemoryMax=134217728\nMemorySwapMax=0\nTasksMax=1\nRuntimeMaxUSec=5s\nTimeoutStopUSec=1s\n"));
        assert!(verify_at(root.path(), &unit, &props).unwrap().enforced);
        props.insert("RuntimeMaxUSec".into(), "5000000us".into());
        assert!(verify_at(root.path(), &unit, &props).is_ok());
        std::fs::write(directory.join("memory.max"), "max").unwrap();
        assert!(verify_at(root.path(), &unit, &props).is_err());
        std::fs::write(directory.join("memory.max"), "134217728").unwrap();
        props.insert("RuntimeMaxUSec".into(), "infinity".into());
        assert!(verify_at(root.path(), &unit, &props).is_err());
        props.insert("RuntimeMaxUSec".into(), "5s".into());
        props.insert("ControlGroup".into(), format!("/../{unit}"));
        assert!(verify_at(root.path(), &unit, &props).is_err());
        assert_eq!(duration_usec("5000ms"), Some(5_000_000));
        assert_eq!(duration_usec("5min"), None);
    }
    #[tokio::test]
    async fn invalid_units_and_non_linux_api_never_launch_a_worker() {
        let cancel = CancellationToken::new();
        assert_eq!(
            stop_orphan("dsh-wasm-*.service").await.unwrap_err().code,
            "invalid_worker_unit"
        );
        if !cfg!(target_os = "linux") {
            let error = probe(&new_unit_name(), &cancel).await.unwrap_err();
            assert_eq!(error.code, "worker_platform_unavailable");
            assert!(error.exit_confirmed);
        }
    }
}
