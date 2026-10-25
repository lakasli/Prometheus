//! The only place that parses, compiles, instantiates or executes generated
//! Component bytes. Launched before Tokio; sandbox installation precedes input.
use crate::plugin_sandbox::{self, Isolation};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
};
use wasmtime::{
    component::{Component, Linker},
    Config, Engine as WasmEngine, Store, StoreLimits, StoreLimitsBuilder,
};

pub const ENGINE: &str = "wasmtime-36.0.19";
pub const HOST_API: &str = "dsh:plugin@1.0.0";
pub const MAX_COMPONENT: usize = 4 * 1024 * 1024;
pub const MAX_SOURCE: usize = 65536;
pub const MAX_INPUT: usize = 16384;
pub const MAX_OUTPUT: usize = 65536;
pub const MAX_PROTOCOL: usize = 6 * 1024 * 1024;
pub const FUEL: u64 = 5_000_000;
pub const WIT: &str = include_str!("../plugin-template/world.wit");
pub const TEMPLATE: &str = include_str!("../plugin-template/guest.rs");

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub input: Value,
    pub expected: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Probe,
    Build {
        body: String,
    },
    Test {
        component_base64: String,
        cases: Vec<Case>,
    },
    Invoke {
        component_base64: String,
        input: Value,
    },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub operation: Operation,
    pub forbidden_canary: String,
    pub forbidden_directory: String,
    pub peer_pid: u32,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub ok: bool,
    pub error_code: Option<String>,
    pub engine: String,
    pub host_api: String,
    pub isolation: Isolation,
    pub probes: Value,
    pub component_base64: Option<String>,
    pub build_environment: Option<Value>,
    pub outputs: Vec<Value>,
}
fn denied(result: std::io::Result<impl Sized>) -> bool {
    result.is_err_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
}
fn probes(work: &Path, request: &Request) -> Value {
    let scratch = work.join("write-probe");
    let scratch_ok = std::fs::write(&scratch, b"scratch").is_ok();
    let _ = std::fs::remove_file(&scratch);
    json!({
        "production_canary_read_denied":denied(std::fs::read(&request.forbidden_canary)),
        "sibling_directory_read_denied":denied(std::fs::read_dir(&request.forbidden_directory)),
        "proc_environ_read_denied":denied(std::fs::read(format!("/proc/{}/environ",request.peer_pid))),
        "proc_fd_read_denied":denied(std::fs::read_dir(format!("/proc/{}/fd",request.peer_pid))),
        "outside_write_denied":denied(std::fs::OpenOptions::new().write(true).create_new(true).open(Path::new(&request.forbidden_directory).join("must-not-write"))),
        "sockets_inet_inet6_unix_denied":plugin_sandbox::socket_probe(),
        "process_read_and_signal_denied":plugin_sandbox::process_probe(request.peer_pid),
        "scratch_write_allowed":scratch_ok,
    })
}
pub fn probes_passed(probes: &Value) -> bool {
    probes
        .as_object()
        .is_some_and(|v| v.len() == 8 && v.values().all(|v| v == &Value::Bool(true)))
}
fn decode(value: &str) -> Result<Vec<u8>, &'static str> {
    if value.len() > MAX_COMPONENT * 4 / 3 + 16 {
        return Err("plugin_component_too_large");
    }
    let bytes = STANDARD
        .decode(value)
        .map_err(|_| "plugin_component_invalid")?;
    if bytes.is_empty() || bytes.len() > MAX_COMPONENT {
        return Err("plugin_component_too_large");
    }
    Ok(bytes)
}
fn build(work: &Path, rustc: &Path, body: &str) -> Result<Vec<u8>, &'static str> {
    if body.trim().is_empty() || body.len() > MAX_SOURCE || body.contains('\0') {
        return Err("plugin_source_invalid");
    }
    let source = TEMPLATE.replace("/* DSH_GENERATED_BODY */", body);
    std::fs::write(work.join("guest.rs"), source).map_err(|_| "plugin_build_workspace")?;
    let status = Command::new(rustc)
        .args([
            "--edition=2021",
            "--crate-name=dsh_generated",
            "--crate-type=cdylib",
            "--target=wasm32-unknown-unknown",
            "-Cpanic=abort",
            "-Copt-level=s",
            "-Clto=no",
            "-Ccodegen-units=1",
            "-Clink-arg=--max-memory=67108864",
            "-Clink-arg=--strip-all",
            "guest.rs",
            "-o",
            "guest.wasm",
        ])
        .current_dir(work)
        .env_clear()
        .env("TMPDIR", work)
        .env("LANG", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| "plugin_compiler_unavailable")?;
    if !status.success() {
        return Err("plugin_rust_compile_failed");
    }
    let mut core = std::fs::read(work.join("guest.wasm")).map_err(|_| "plugin_artifact_missing")?;
    if core.len() > MAX_COMPONENT {
        return Err("plugin_component_too_large");
    }
    // There is deliberately no WASI adapter. Unknown ambient imports fail.
    for payload in wasmparser::Parser::new(0).parse_all(&core) {
        if let wasmparser::Payload::ImportSection(section) =
            payload.map_err(|_| "plugin_core_invalid")?
        {
            if section.count() != 0 {
                return Err("plugin_imports_forbidden");
            }
        }
    }
    let mut resolve = wit_parser::Resolve::default();
    let package = resolve
        .push_str("world.wit", WIT)
        .map_err(|_| "plugin_wit_invalid")?;
    let world = resolve
        .select_world(package, Some("plugin"))
        .map_err(|_| "plugin_wit_invalid")?;
    wit_component::embed_component_metadata(
        &mut core,
        &resolve,
        world,
        wit_component::StringEncoding::UTF8,
    )
    .map_err(|_| "plugin_metadata_failed")?;
    let component = wit_component::ComponentEncoder::default()
        .module(&core)
        .map_err(|_| "plugin_component_invalid")?
        .validate(true)
        .encode()
        .map_err(|_| "plugin_component_invalid")?;
    if component.len() > MAX_COMPONENT {
        return Err("plugin_component_too_large");
    }
    Ok(component)
}
fn execute(bytes: &[u8], inputs: &[Value]) -> Result<Vec<Value>, &'static str> {
    let mut config = Config::new();
    config
        .wasm_component_model(true)
        .consume_fuel(true)
        .epoch_interruption(true)
        .max_wasm_stack(256 * 1024)
        .memory_reservation(64 * 1024 * 1024)
        .memory_guard_size(65536);
    let engine = WasmEngine::new(&config).map_err(|_| "plugin_engine_unavailable")?;
    let timer_engine = engine.clone();
    let _ = std::thread::Builder::new()
        .name("plugin-deadline".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(15));
            timer_engine.increment_epoch();
        })
        .map_err(|_| "plugin_deadline_unavailable")?;
    let component = Component::new(&engine, bytes).map_err(|_| "plugin_component_invalid")?;
    let linker: Linker<StoreLimits> = Linker::new(&engine); // No host API or WASI linker.
    let mut outputs = Vec::new();
    for input in inputs {
        let input = serde_json::to_string(input).map_err(|_| "plugin_input_invalid")?;
        if input.len() > MAX_INPUT {
            return Err("plugin_input_too_large");
        }
        let limits = StoreLimitsBuilder::new()
            .memory_size(64 * 1024 * 1024)
            .instances(1)
            .memories(1)
            .table_elements(4096)
            .trap_on_grow_failure(true)
            .build();
        let mut store = Store::new(&engine, limits);
        store.limiter(|limits| limits);
        store
            .set_fuel(FUEL)
            .map_err(|_| "plugin_fuel_unavailable")?;
        store.set_epoch_deadline(1);
        let instance = linker
            .instantiate(&mut store, &component)
            .map_err(|_| "plugin_import_or_instance_rejected")?;
        let health = instance
            .get_typed_func::<(), (bool,)>(&mut store, "health")
            .map_err(|_| "plugin_contract_invalid")?;
        let (healthy,) = health
            .call(&mut store, ())
            .map_err(|_| "plugin_health_failed")?;
        health
            .post_return(&mut store)
            .map_err(|_| "plugin_health_failed")?;
        if !healthy {
            return Err("plugin_health_failed");
        }
        let invoke = instance
            .get_typed_func::<(String,), (String,)>(&mut store, "invoke")
            .map_err(|_| "plugin_contract_invalid")?;
        let (output,) = invoke.call(&mut store, (input,)).map_err(|error| {
            if error.downcast_ref::<wasmtime::Trap>() == Some(&wasmtime::Trap::OutOfFuel) {
                "plugin_out_of_fuel"
            } else {
                "plugin_guest_trap"
            }
        })?;
        invoke
            .post_return(&mut store)
            .map_err(|_| "plugin_post_return_failed")?;
        if output.len() > MAX_OUTPUT {
            return Err("plugin_output_too_large");
        }
        let output = serde_json::from_str(&output).map_err(|_| "plugin_output_not_json")?;
        outputs.push(output);
    }
    Ok(outputs)
}
fn operation(
    work: &Path,
    rustc: &Path,
    op: &Operation,
) -> Result<(Option<String>, Vec<Value>), &'static str> {
    match op {
        Operation::Probe => Ok((None, vec![])),
        Operation::Build { body } => {
            let component = build(work, rustc, body)?;
            // Type check and health + benign invoke form part of a separate test,
            // not approval. Compilation alone never executes generated code.
            Ok((Some(STANDARD.encode(component)), vec![]))
        }
        Operation::Test {
            component_base64,
            cases,
        } => {
            if !(2..=8).contains(&cases.len()) {
                return Err("plugin_tests_invalid");
            }
            let outputs = execute(
                &decode(component_base64)?,
                &cases.iter().map(|v| v.input.clone()).collect::<Vec<_>>(),
            )?;
            Ok((None, outputs))
        }
        Operation::Invoke {
            component_base64,
            input,
        } => Ok((
            None,
            execute(&decode(component_base64)?, std::slice::from_ref(input))?,
        )),
    }
}
pub fn run_stdio(work: &Path, rustc: &Path) -> i32 {
    let result = (|| -> Result<Response, &'static str> {
        let isolation = plugin_sandbox::install(work, rustc)?;
        let mut bytes = Vec::new();
        std::io::stdin()
            .take((MAX_PROTOCOL + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| "plugin_protocol_invalid")?;
        if bytes.len() > MAX_PROTOCOL {
            return Err("plugin_protocol_too_large");
        }
        let request: Request =
            serde_json::from_slice(&bytes).map_err(|_| "plugin_protocol_invalid")?;
        let probes = probes(work, &request);
        let build_environment = if probes_passed(&probes)
            && matches!(request.operation, Operation::Build { .. })
        {
            let version = Command::new(rustc)
                .arg("--version")
                .env_clear()
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .map_err(|_| "plugin_compiler_unavailable")?;
            if !version.status.success() || version.stdout.len() > 512 {
                return Err("plugin_compiler_unavailable");
            }
            let compiler = std::fs::read(rustc).map_err(|_| "plugin_compiler_unavailable")?;
            Some(
                json!({"rustc_version":String::from_utf8(version.stdout).map_err(|_|"plugin_compiler_unavailable")?.trim(),"rustc_sha256":hex::encode(Sha256::digest(&compiler)),"template_sha256":hex::encode(Sha256::digest(TEMPLATE.as_bytes())),"wit_sha256":hex::encode(Sha256::digest(WIT.as_bytes())),"target":"wasm32-unknown-unknown","dependencies":[],"cargo_build_scripts":false,"network":false,"policy":plugin_sandbox::POLICY}),
            )
        } else {
            None
        };
        let result = if probes_passed(&probes) {
            operation(work, rustc, &request.operation)
        } else {
            Err("plugin_sandbox_probe_failed")
        };
        let (ok, error_code, component_base64, outputs) = match result {
            Ok((component, outputs)) => (true, None, component, outputs),
            Err(code) => (false, Some(code.into()), None, vec![]),
        };
        Ok(Response {
            ok,
            error_code,
            engine: ENGINE.into(),
            host_api: HOST_API.into(),
            isolation,
            probes,
            component_base64,
            build_environment,
            outputs,
        })
    })();
    match result {
        Ok(response) => {
            let Ok(bytes) = serde_json::to_vec(&response) else {
                return 70;
            };
            if bytes.len() > MAX_PROTOCOL {
                return 70;
            }
            if std::io::stdout().write_all(&bytes).is_err() {
                return 70;
            }
            if response.ok {
                0
            } else {
                1
            }
        }
        Err(code) => {
            // The pre-sandbox error only contains a fixed code, never source,
            // compiler stderr, paths, tokens or guest-produced diagnostics.
            let _ =
                std::io::stdout().write_all(json!({"preflight_error":code}).to_string().as_bytes());
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(invocation: &str, pointer: u32, len: u32, output: &str) -> Vec<u8> {
        let header = [pointer.to_le_bytes(), len.to_le_bytes()]
            .concat()
            .iter()
            .map(|b| format!("\\{b:02x}"))
            .collect::<String>();
        let data = output
            .bytes()
            .map(|b| format!("\\{b:02x}"))
            .collect::<String>();
        let mut core = wat::parse_str(format!(
            r#"(module
          (memory (export "memory") 2 1024)
          (data (i32.const 0) "{header}")
          (data (i32.const 64) "{data}")
          (func (export "health") (result i32) i32.const 1)
          (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) i32.const 4096)
          (func (export "invoke") (param i32 i32) (result i32) {invocation})
        )"#
        ))
        .unwrap();
        let mut resolve = wit_parser::Resolve::default();
        let package = resolve.push_str("world.wit", WIT).unwrap();
        let world = resolve.select_world(package, Some("plugin")).unwrap();
        wit_component::embed_component_metadata(
            &mut core,
            &resolve,
            world,
            wit_component::StringEncoding::UTF8,
        )
        .unwrap();
        wit_component::ComponentEncoder::default()
            .module(&core)
            .unwrap()
            .validate(true)
            .encode()
            .unwrap()
    }
    #[test]
    fn actual_component_contract_returns_json() {
        let bytes = fixture("i32.const 0", 64, 9, "\"fixture\"");
        assert_eq!(
            execute(&bytes, &[json!({})]).unwrap(),
            vec![json!("fixture")]
        );
    }
    #[test]
    fn component_fuel_memory_imports_and_output_limits_fail_closed() {
        assert_eq!(
            execute(
                &fixture("(loop br 0) i32.const 0", 64, 4, "null"),
                &[json!({})]
            )
            .unwrap_err(),
            "plugin_out_of_fuel"
        );
        assert_eq!(
            execute(
                &fixture("i32.const 2000 memory.grow drop i32.const 0", 64, 4, "null"),
                &[json!({})]
            )
            .unwrap_err(),
            "plugin_guest_trap"
        );
        assert_eq!(
            execute(&fixture("i32.const 0", 64, 65537, "null"), &[json!({})]).unwrap_err(),
            "plugin_output_too_large"
        );
        assert_eq!(
            execute(&fixture("i32.const 0", 64, 4, "oops"), &[json!({})]).unwrap_err(),
            "plugin_output_not_json"
        );
        let forbidden = wat::parse_str("(component (import \"forbidden-host\" (func)))").unwrap();
        assert_eq!(
            execute(&forbidden, &[json!({})]).unwrap_err(),
            "plugin_import_or_instance_rejected"
        );
    }
}
