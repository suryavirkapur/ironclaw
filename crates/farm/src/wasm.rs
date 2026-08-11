//! Sandboxed executor for Ironclaw Wasm tools.
//!
//! Modules receive no WASI environment and no host imports. The ABI is:
//! - export `memory`
//! - export `ironclaw_alloc(len: i32) -> i32`
//! - export `ironclaw_run(input_ptr: i32, input_len: i32) -> i64`
//!
//! `ironclaw_run` returns `(output_ptr << 32) | output_len`; input and output
//! are UTF-8 JSON. Capability-bearing host imports will be added separately so
//! a module can never acquire MCP, A2A, filesystem, or network access merely by
//! being loaded.

use crate::manifest::WasmTool;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;
use wasmtime::{Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};

const MAX_JSON_BYTES: usize = 16 * 1024 * 1024;

pub struct WasmExecutor {
    engine: Engine,
    tools_root: PathBuf,
}

struct StoreState {
    limits: StoreLimits,
}

impl WasmExecutor {
    pub fn new(tools_root: PathBuf) -> Result<Self, WasmRuntimeError> {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.epoch_interruption(true);
        let engine = Engine::new(&config).map_err(WasmRuntimeError::engine)?;
        Ok(Self { engine, tools_root })
    }

    pub fn invoke(&self, tool: &WasmTool, input: &Value) -> Result<Value, WasmRuntimeError> {
        let module_path = resolve_module(&self.tools_root, &tool.module)?;
        let module = Module::from_file(&self.engine, &module_path)
            .map_err(|err| WasmRuntimeError::Load(module_path.clone(), err.to_string()))?;

        let memory_limit = usize::try_from(tool.limits.memory_mib)
            .unwrap_or(usize::MAX)
            .saturating_mul(1024 * 1024);
        let limits = StoreLimitsBuilder::new().memory_size(memory_limit).build();
        let mut store = Store::new(&self.engine, StoreState { limits });
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(tool.limits.fuel)
            .map_err(WasmRuntimeError::engine)?;
        store.set_epoch_deadline(1);

        let engine = self.engine.clone();
        let timeout = Duration::from_millis(tool.limits.timeout_ms);
        std::thread::spawn(move || {
            std::thread::sleep(timeout);
            engine.increment_epoch();
        });

        let linker = Linker::new(&self.engine);
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(WasmRuntimeError::instantiate)?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or(WasmRuntimeError::MissingExport("memory"))?;
        let alloc = instance
            .get_typed_func::<i32, i32>(&mut store, "ironclaw_alloc")
            .map_err(|_| WasmRuntimeError::MissingExport("ironclaw_alloc"))?;
        let run = instance
            .get_typed_func::<(i32, i32), i64>(&mut store, "ironclaw_run")
            .map_err(|_| WasmRuntimeError::MissingExport("ironclaw_run"))?;

        let encoded = serde_json::to_vec(input).map_err(WasmRuntimeError::Json)?;
        if encoded.len() > MAX_JSON_BYTES {
            return Err(WasmRuntimeError::OutputTooLarge(encoded.len()));
        }
        let input_len = i32::try_from(encoded.len())
            .map_err(|_| WasmRuntimeError::OutputTooLarge(encoded.len()))?;
        let input_ptr = alloc
            .call(&mut store, input_len)
            .map_err(WasmRuntimeError::trap)?;
        if input_ptr < 0 {
            return Err(WasmRuntimeError::InvalidPointer(input_ptr as i64));
        }
        memory
            .write(&mut store, input_ptr as usize, &encoded)
            .map_err(WasmRuntimeError::memory)?;

        let packed = run
            .call(&mut store, (input_ptr, input_len))
            .map_err(WasmRuntimeError::trap)? as u64;
        let output_ptr = (packed >> 32) as u32 as usize;
        let output_len = (packed & u32::MAX as u64) as u32 as usize;
        if output_len > MAX_JSON_BYTES {
            return Err(WasmRuntimeError::OutputTooLarge(output_len));
        }
        let mut output = vec![0; output_len];
        memory
            .read(&store, output_ptr, &mut output)
            .map_err(WasmRuntimeError::memory)?;
        serde_json::from_slice(&output).map_err(WasmRuntimeError::Json)
    }
}

fn resolve_module(root: &Path, relative: &Path) -> Result<PathBuf, WasmRuntimeError> {
    let canonical_root = root
        .canonicalize()
        .map_err(|err| WasmRuntimeError::Path(root.to_path_buf(), err.to_string()))?;
    let candidate = canonical_root.join(relative);
    let canonical_module = candidate
        .canonicalize()
        .map_err(|err| WasmRuntimeError::Path(candidate.clone(), err.to_string()))?;
    if !canonical_module.starts_with(&canonical_root) {
        return Err(WasmRuntimeError::Path(
            candidate,
            "module escapes the agent tools directory".to_string(),
        ));
    }
    Ok(canonical_module)
}

#[derive(Debug, thiserror::Error)]
pub enum WasmRuntimeError {
    #[error("Wasm engine error: {0}")]
    Engine(String),
    #[error("failed to load Wasm module {0}: {1}")]
    Load(PathBuf, String),
    #[error("failed to instantiate Wasm module: {0}")]
    Instantiate(String),
    #[error("Wasm module is missing required export {0}")]
    MissingExport(&'static str),
    #[error("Wasm module trapped: {0}")]
    Trap(String),
    #[error("Wasm memory access failed: {0}")]
    Memory(String),
    #[error("Wasm module returned invalid pointer {0}")]
    InvalidPointer(i64),
    #[error("Wasm JSON input or output is invalid: {0}")]
    Json(serde_json::Error),
    #[error("Wasm JSON buffer exceeds limit: {0} bytes")]
    OutputTooLarge(usize),
    #[error("invalid Wasm module path {0}: {1}")]
    Path(PathBuf, String),
}

impl WasmRuntimeError {
    fn engine(error: impl std::fmt::Display) -> Self {
        Self::Engine(error.to_string())
    }
    fn instantiate(error: impl std::fmt::Display) -> Self {
        Self::Instantiate(error.to_string())
    }
    fn trap(error: impl std::fmt::Display) -> Self {
        Self::Trap(error.to_string())
    }
    fn memory(error: impl std::fmt::Display) -> Self {
        Self::Memory(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::CapabilityEffect;
    use crate::manifest::{WasmLimits, WasmPermissions};
    use serde_json::json;

    #[test]
    fn executes_json_abi_without_wasi() {
        let temp = tempfile::tempdir().unwrap();
        let module = wat::parse_str(
            r#"
            (module
              (memory (export "memory") 1)
              (global $next (mut i32) (i32.const 1024))
              (func (export "ironclaw_alloc") (param $len i32) (result i32)
                (local $ptr i32)
                (local.set $ptr (global.get $next))
                (global.set $next (i32.add (global.get $next) (local.get $len)))
                (local.get $ptr))
              (data (i32.const 16) "{\22ok\22:true}")
              (func (export "ironclaw_run") (param i32 i32) (result i64)
                (i64.or
                  (i64.shl (i64.const 16) (i64.const 32))
                  (i64.const 11))))
            "#,
        )
        .unwrap();
        std::fs::write(temp.path().join("echo.wasm"), module).unwrap();
        let tool = WasmTool {
            id: "echo".to_string(),
            module: PathBuf::from("echo.wasm"),
            description: "test".to_string(),
            input_schema: Value::Null,
            output_schema: Value::Null,
            effect: CapabilityEffect::Read,
            data_classes: Vec::new(),
            requires_approval: false,
            permissions: WasmPermissions::default(),
            limits: WasmLimits::default(),
        };
        let executor = WasmExecutor::new(temp.path().to_path_buf()).unwrap();
        assert_eq!(
            executor.invoke(&tool, &json!({})).unwrap(),
            json!({"ok": true})
        );
    }
}
