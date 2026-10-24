//! Synchronous import-free guest VM; only used in the fixed wasm-worker process.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

pub const ENGINE: &str = "wasmi-2.0.0";
pub const POLICY: &str = "pure-i32-v1";
pub const MAX_CODE_BYTES: usize = 65536;
pub const MAX_REQUEST_BYTES: usize = 512 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 128 * 1024;
pub const MEMORY_MAX_BYTES: u64 = 128 * 1024 * 1024;
pub const FUEL: u64 = 100_000;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub args: [i32; 2],
    pub expected: i32,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResourceInfo {
    pub enforced: bool,
    pub memory_max_bytes: Option<u64>,
    pub swap_max_bytes: Option<u64>,
    pub pids_max: Option<u64>,
    pub unit: Option<String>,
}
impl ResourceInfo {
    fn local() -> Self {
        Self {
            enforced: false,
            memory_max_bytes: None,
            swap_max_bytes: None,
            pids_max: None,
            unit: None,
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResourceProbeKind {
    Memory,
    Timeout,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Probe,
    CompileAndTest { wat: String, cases: Vec<Case> },
    Execute { wasm_base64: String, args: [i32; 2] },
    ResourceProbe { kind: ResourceProbeKind },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Probe,
    CompileAndTest {
        wasm_base64: String,
        wasm_sha256: String,
        wat_sha256: String,
        results: Vec<i32>,
    },
    Execute {
        result: i32,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Ok {
        engine: String,
        policy: String,
        resources: ResourceInfo,
        outcome: Box<Outcome>,
    },
    Error {
        code: ErrorCode,
    },
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    ProtocolError,
    InputTooLarge,
    InvalidWat,
    InvalidWasm,
    PolicyRejected,
    InvalidTests,
    TestFailed,
    OutOfFuel,
    StackLimit,
    GuestTrap,
    ResourceUnavailable,
    InternalError,
}
impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProtocolError => "worker_protocol_error",
            Self::InputTooLarge => "code_too_large",
            Self::InvalidWat => "invalid_wat",
            Self::InvalidWasm => "invalid_wasm",
            Self::PolicyRejected => "wasm_policy_rejected",
            Self::InvalidTests => "invalid_tests",
            Self::TestFailed => "wasm_test_failed",
            Self::OutOfFuel => "wasm_out_of_fuel",
            Self::StackLimit => "wasm_stack_limit",
            Self::GuestTrap => "wasm_guest_trap",
            Self::ResourceUnavailable => "worker_resource_unavailable",
            Self::InternalError => "worker_internal_error",
        }
    }
}
pub fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
pub fn valid_unit_name(unit: &str) -> bool {
    let Some(id) = unit
        .strip_prefix("dsh-wasm-")
        .and_then(|s| s.strip_suffix(".service"))
    else {
        return false;
    };
    uuid::Uuid::parse_str(id).is_ok_and(|v| v.get_version_num() == 4 && v.to_string() == id)
}
#[cfg(target_os = "linux")]
fn enforced_resources() -> Result<ResourceInfo, ErrorCode> {
    use std::path::{Component, Path};
    let membership =
        std::fs::read_to_string("/proc/self/cgroup").map_err(|_| ErrorCode::ResourceUnavailable)?;
    let relative = membership
        .lines()
        .find_map(|s| s.strip_prefix("0::"))
        .ok_or(ErrorCode::ResourceUnavailable)?;
    let path = Path::new(relative);
    if !path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
    {
        return Err(ErrorCode::ResourceUnavailable);
    }
    let unit = path
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| valid_unit_name(s))
        .ok_or(ErrorCode::ResourceUnavailable)?;
    let root = Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/'));
    let number = |file: &str| -> Result<u64, ErrorCode> {
        std::fs::read_to_string(root.join(file))
            .map_err(|_| ErrorCode::ResourceUnavailable)?
            .trim()
            .parse()
            .map_err(|_| ErrorCode::ResourceUnavailable)
    };
    let memory = number("memory.max")?;
    let swap = number("memory.swap.max")?;
    let pids = number("pids.max")?;
    if memory != MEMORY_MAX_BYTES || swap != 0 || pids != 1 {
        return Err(ErrorCode::ResourceUnavailable);
    }
    Ok(ResourceInfo {
        enforced: true,
        memory_max_bytes: Some(memory),
        swap_max_bytes: Some(swap),
        pids_max: Some(pids),
        unit: Some(unit.into()),
    })
}
#[cfg(not(target_os = "linux"))]
fn enforced_resources() -> Result<ResourceInfo, ErrorCode> {
    Err(ErrorCode::ResourceUnavailable)
}

fn inspect(wasm: &[u8]) -> Result<(), ErrorCode> {
    use wasmparser::{Encoding, ExternalKind, Parser, Payload, ValType};
    if wasm.is_empty() || wasm.len() > MAX_CODE_BYTES {
        return Err(ErrorCode::InputTooLarge);
    }
    let mut types = Vec::new();
    let mut functions = Vec::new();
    let mut exported = None;
    let mut bodies = 0usize;
    for payload in Parser::new(0).parse_all(wasm) {
        match payload.map_err(|_| ErrorCode::InvalidWasm)? {
            Payload::Version {
                encoding: Encoding::Module,
                ..
            } => {}
            Payload::TypeSection(section) => {
                if section.count() > 16 {
                    return Err(ErrorCode::PolicyRejected);
                }
                for ty in section.into_iter_err_on_gc_types() {
                    let ty = ty.map_err(|_| ErrorCode::PolicyRejected)?;
                    if ty.params().len() > 8
                        || ty.results().len() > 1
                        || ty
                            .params()
                            .iter()
                            .chain(ty.results())
                            .any(|v| *v != ValType::I32)
                    {
                        return Err(ErrorCode::PolicyRejected);
                    }
                    types.push(ty);
                }
            }
            Payload::ImportSection(section) => {
                if section.count() != 0 {
                    return Err(ErrorCode::PolicyRejected);
                }
            }
            Payload::FunctionSection(section) => {
                if section.count() == 0 || section.count() > 16 {
                    return Err(ErrorCode::PolicyRejected);
                }
                for ty in section {
                    functions.push(ty.map_err(|_| ErrorCode::InvalidWasm)?);
                }
            }
            Payload::ExportSection(section) => {
                if section.count() != 1 {
                    return Err(ErrorCode::PolicyRejected);
                }
                for export in section {
                    let export = export.map_err(|_| ErrorCode::InvalidWasm)?;
                    if export.name != "calc" || export.kind != ExternalKind::Func {
                        return Err(ErrorCode::PolicyRejected);
                    }
                    exported = Some(export.index as usize);
                }
            }
            Payload::CodeSectionStart { count, .. } => {
                if count == 0 || count > 16 {
                    return Err(ErrorCode::PolicyRejected);
                }
            }
            Payload::CodeSectionEntry(body) => {
                bodies += 1;
                let mut locals = 0u32;
                for local in body
                    .get_locals_reader()
                    .map_err(|_| ErrorCode::InvalidWasm)?
                {
                    let (count, ty) = local.map_err(|_| ErrorCode::InvalidWasm)?;
                    locals = locals.checked_add(count).ok_or(ErrorCode::PolicyRejected)?;
                    if locals > 64 || ty != ValType::I32 {
                        return Err(ErrorCode::PolicyRejected);
                    }
                }
            }
            Payload::CustomSection(_) => {} // bounded names/debug bytes are ignored by the engine
            Payload::End(_) => {}
            _ => return Err(ErrorCode::PolicyRejected), // no memory/table/global/start/data/element/tag/component
        }
    }
    if bodies != functions.len() || bodies == 0 {
        return Err(ErrorCode::InvalidWasm);
    }
    let function = exported
        .and_then(|i| functions.get(i))
        .and_then(|i| types.get(*i as usize))
        .ok_or(ErrorCode::PolicyRejected)?;
    if function.params() != [ValType::I32, ValType::I32] || function.results() != [ValType::I32] {
        return Err(ErrorCode::PolicyRejected);
    }
    Ok(())
}
fn engine() -> wasmi::Engine {
    let mut config = wasmi::Config::default();
    config
        .consume_fuel(true)
        .allow_start_fn(false)
        .floats(false)
        .ignore_custom_sections(true)
        .wasm_multi_value(false)
        .wasm_multi_memory(false)
        .wasm_reference_types(false)
        .wasm_mutable_global(false)
        .wasm_tail_call(false)
        .wasm_bulk_memory(false)
        .wasm_extended_const(false)
        .set_min_stack_height(4096)
        .set_max_stack_height(65536)
        .set_max_recursion_depth(32)
        .set_max_cached_stacks(0)
        .enforced_limits(wasmi::EnforcedLimits::strict())
        .compilation_mode(wasmi::CompilationMode::Eager);
    wasmi::Engine::new(&config)
}
fn guest_error(error: wasmi::Error) -> ErrorCode {
    match error.as_trap_code() {
        Some(wasmi::TrapCode::OutOfFuel) => ErrorCode::OutOfFuel,
        Some(wasmi::TrapCode::StackOverflow) => ErrorCode::StackLimit,
        _ => ErrorCode::GuestTrap,
    }
}
fn call(engine: &wasmi::Engine, module: &wasmi::Module, args: [i32; 2]) -> Result<i32, ErrorCode> {
    let limits = wasmi::StoreLimitsBuilder::new()
        .instances(1)
        .memories(0)
        .tables(0)
        .memory_size(0)
        .table_elements(0)
        .trap_on_grow_failure(true)
        .build();
    let mut store = wasmi::Store::new(engine, limits);
    store.limiter(|limits| limits);
    store.set_fuel(FUEL).map_err(|_| ErrorCode::InternalError)?;
    let instance = wasmi::Linker::new(engine)
        .instantiate_and_start(&mut store, module)
        .map_err(guest_error)?;
    let function = instance
        .get_typed_func::<(i32, i32), i32>(&store, "calc")
        .map_err(|_| ErrorCode::PolicyRejected)?;
    function
        .call(&mut store, (args[0], args[1]))
        .map_err(guest_error)
}
fn process(request: Request, resources: &ResourceInfo) -> Result<Outcome, ErrorCode> {
    match request {
        Request::Probe => Ok(Outcome::Probe),
        Request::CompileAndTest { wat, cases } => {
            if wat.is_empty() || wat.len() > MAX_CODE_BYTES {
                return Err(ErrorCode::InputTooLarge);
            }
            if !(2..=8).contains(&cases.len()) {
                return Err(ErrorCode::InvalidTests);
            }
            let wasm = wat::parse_str(&wat).map_err(|_| ErrorCode::InvalidWat)?;
            inspect(&wasm)?;
            let engine = engine();
            let module =
                wasmi::Module::new(&engine, &wasm[..]).map_err(|_| ErrorCode::PolicyRejected)?;
            let mut results = Vec::new();
            for case in cases {
                let result = call(&engine, &module, case.args)?;
                if result != case.expected {
                    return Err(ErrorCode::TestFailed);
                }
                results.push(result);
            }
            Ok(Outcome::CompileAndTest {
                wasm_base64: STANDARD.encode(&wasm),
                wasm_sha256: sha256(&wasm),
                wat_sha256: sha256(wat.as_bytes()),
                results,
            })
        }
        Request::Execute { wasm_base64, args } => {
            if wasm_base64.len() > MAX_CODE_BYTES.div_ceil(3) * 4 {
                return Err(ErrorCode::InputTooLarge);
            }
            let wasm = STANDARD
                .decode(wasm_base64)
                .map_err(|_| ErrorCode::InvalidWasm)?;
            inspect(&wasm)?;
            let engine = engine();
            let module =
                wasmi::Module::new(&engine, &wasm[..]).map_err(|_| ErrorCode::PolicyRejected)?;
            Ok(Outcome::Execute {
                result: call(&engine, &module, args)?,
            })
        }
        Request::ResourceProbe { kind } => {
            if !resources.enforced {
                return Err(ErrorCode::ResourceUnavailable);
            }
            match kind {
                ResourceProbeKind::Memory => {
                    let memory = vec![0x5a_u8; 192 * 1024 * 1024];
                    std::hint::black_box(&memory);
                    Err(ErrorCode::ResourceUnavailable)
                }
                ResourceProbeKind::Timeout => loop {
                    std::hint::black_box(1u64.wrapping_add(2));
                },
            }
        }
    }
}
fn respond(response: &Response, mut output: impl Write) -> i32 {
    let Ok(bytes) = serde_json::to_vec(response) else {
        return 2;
    };
    if bytes.len() > MAX_RESPONSE_BYTES
        || output
            .write_all(&bytes)
            .and_then(|_| output.write_all(b"\n"))
            .and_then(|_| output.flush())
            .is_err()
    {
        return 2;
    }
    if matches!(response, Response::Ok { .. }) {
        0
    } else {
        2
    }
}
pub fn run_stdio(require_cgroup: bool) -> i32 {
    // Actual kernel limits are checked before any untrusted input is read.
    let resources = if require_cgroup {
        match enforced_resources() {
            Ok(resources) => resources,
            Err(code) => return respond(&Response::Error { code }, std::io::stdout().lock()),
        }
    } else {
        ResourceInfo::local()
    };
    let mut input = Vec::new();
    if std::io::stdin()
        .lock()
        .take((MAX_REQUEST_BYTES + 1) as u64)
        .read_to_end(&mut input)
        .is_err()
    {
        return respond(
            &Response::Error {
                code: ErrorCode::ProtocolError,
            },
            std::io::stdout().lock(),
        );
    }
    let result = if input.len() > MAX_REQUEST_BYTES {
        Err(ErrorCode::InputTooLarge)
    } else {
        serde_json::from_slice::<Request>(&input)
            .map_err(|_| ErrorCode::ProtocolError)
            .and_then(|request| process(request, &resources))
    };
    let response = match result {
        Ok(outcome) => Response::Ok {
            engine: ENGINE.into(),
            policy: POLICY.into(),
            resources,
            outcome: Box::new(outcome),
        },
        Err(code) => Response::Error { code },
    };
    respond(&response, std::io::stdout().lock())
}

#[cfg(test)]
mod tests {
    use super::*;
    const ADD:&str="(module (func (export \"calc\") (param i32 i32) (result i32) local.get 0 local.get 1 i32.add))";
    fn compile(wat: &str) -> Result<Outcome, ErrorCode> {
        process(
            Request::CompileAndTest {
                wat: wat.into(),
                cases: vec![
                    Case {
                        args: [2, 3],
                        expected: 5,
                    },
                    Case {
                        args: [-7, 2],
                        expected: -5,
                    },
                ],
            },
            &ResourceInfo::local(),
        )
    }
    #[test]
    fn actual_compile_test_and_execute_revalidate_the_artifact() {
        let Outcome::CompileAndTest {
            wasm_base64,
            wasm_sha256,
            wat_sha256,
            results,
        } = compile(ADD).unwrap()
        else {
            panic!()
        };
        assert_eq!(results, [5, -5]);
        assert_eq!(wat_sha256, sha256(ADD.as_bytes()));
        assert_eq!(wasm_sha256, sha256(&STANDARD.decode(&wasm_base64).unwrap()));
        assert!(matches!(
            process(
                Request::Execute {
                    wasm_base64,
                    args: [9, 4]
                },
                &ResourceInfo::local()
            )
            .unwrap(),
            Outcome::Execute { result: 13 }
        ));
    }
    #[test]
    fn no_host_abilities_or_bad_abis_can_pass_policy() {
        for wat in [
            "(module (import \"wasi_snapshot_preview1\" \"proc_exit\" (func)) (func (export \"calc\") (param i32 i32) (result i32) i32.const 0))",
            "(module (memory 1) (func (export \"calc\") (param i32 i32) (result i32) i32.const 0))",
            "(module (table 1 funcref) (func (export \"calc\") (param i32 i32) (result i32) i32.const 0))",
            "(module (global i32 (i32.const 0)) (func (export \"calc\") (param i32 i32) (result i32) i32.const 0))",
            "(module (func $start) (start $start) (func (export \"calc\") (param i32 i32) (result i32) i32.const 0))",
            "(module (func (export \"wrong\") (param i32 i32) (result i32) i32.const 0))",
            "(module (func (export \"calc\") (export \"extra\") (param i32 i32) (result i32) i32.const 0))",
            "(module (func (export \"calc\") (param f64 f64) (result i32) i32.const 0))",
            "(module (func (export \"calc\") (param i32 i32) (result i32) f64.const 1 drop i32.const 0))",
            "(component)",
        ]{assert!(compile(wat).is_err(),"forbidden guest module was accepted");}
    }
    #[test]
    fn loops_recursion_functions_locals_and_code_size_have_real_bounds() {
        assert_eq!(compile("(module (func (export \"calc\") (param i32 i32) (result i32) (loop $again br $again) unreachable))").unwrap_err(),ErrorCode::OutOfFuel);
        assert!(matches!(compile("(module (func $f (export \"calc\") (param i32 i32) (result i32) local.get 0 local.get 1 call $f))"),Err(ErrorCode::StackLimit)|Err(ErrorCode::OutOfFuel)));
        let locals = format!(
            "(module (func (export \"calc\") (param i32 i32) (result i32) (local {}) i32.const 0))",
            "i32 ".repeat(65)
        );
        assert_eq!(compile(&locals).unwrap_err(), ErrorCode::PolicyRejected);
        let functions = format!(
            "(module {} (func (export \"calc\") (param i32 i32) (result i32) i32.const 0))",
            "(func)".repeat(16)
        );
        assert_eq!(compile(&functions).unwrap_err(), ErrorCode::PolicyRejected);
        assert_eq!(
            compile(&" ".repeat(MAX_CODE_BYTES + 1)).unwrap_err(),
            ErrorCode::InputTooLarge
        );
        assert_eq!(
            compile("not-wat private-do-not-echo").unwrap_err(),
            ErrorCode::InvalidWat
        );
    }
    #[test]
    fn independent_tests_must_pass_and_errors_do_not_contain_source() {
        assert_eq!(
            process(
                Request::CompileAndTest {
                    wat: ADD.into(),
                    cases: vec![
                        Case {
                            args: [2, 3],
                            expected: 4
                        };
                        2
                    ]
                },
                &ResourceInfo::local()
            )
            .unwrap_err(),
            ErrorCode::TestFailed
        );
        assert_eq!(
            process(
                Request::CompileAndTest {
                    wat: ADD.into(),
                    cases: vec![Case {
                        args: [2, 3],
                        expected: 5
                    }]
                },
                &ResourceInfo::local()
            )
            .unwrap_err(),
            ErrorCode::InvalidTests
        );
        assert!(serde_json::from_str::<Request>(
            r#"{"op":"execute","wasm_base64":"","args":[1,2],"path":"private"}"#
        )
        .is_err());
        let mut bytes = Vec::new();
        assert_eq!(
            respond(
                &Response::Error {
                    code: ErrorCode::InvalidWat
                },
                &mut bytes
            ),
            2
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            serde_json::json!({"status":"error","code":"invalid_wat"})
        );
        assert_eq!(
            process(
                Request::ResourceProbe {
                    kind: ResourceProbeKind::Memory
                },
                &ResourceInfo::local()
            )
            .unwrap_err(),
            ErrorCode::ResourceUnavailable
        );
    }
    #[test]
    fn exact_worker_unit_names_reject_globs_and_arbitrary_services() {
        assert!(valid_unit_name(&format!(
            "dsh-wasm-{}.service",
            uuid::Uuid::new_v4()
        )));
        for unit in [
            "dsh-wasm-*.service",
            "other.service",
            "../dsh-wasm-x.service",
            "dsh-wasm-00000000-0000-0000-0000-000000000000.service",
        ] {
            assert!(!valid_unit_name(unit));
        }
    }
}
