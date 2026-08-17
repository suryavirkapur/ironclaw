//! Declarative control-plane primitives for an Ironclaw agent farm.
//!
//! This crate deliberately contains no Firecracker, MCP transport, or A2A HTTP
//! implementation. It is the policy boundary shared by those adapters: agent
//! manifests compile into a registry of capabilities, and every invocation is
//! authorized against that registry before an adapter receives it.

pub mod artifact;
pub mod capability;
pub mod manifest;
pub mod reconcile;
pub mod registry;
pub mod router;
pub mod task;
pub mod trace;
#[cfg(feature = "wasm-runtime")]
pub mod wasm;

pub use artifact::{ArtifactError, ArtifactRecord, ArtifactStore};
pub use capability::{Capability, CapabilityEffect, CapabilityKind, CapabilityUri};
pub use manifest::{AgentManifest, ManifestError};
pub use reconcile::{AgentRuntimeState, ReconcileAction, ReconcilePlan, RuntimePhase};
pub use registry::{AgentRecord, FarmRegistry};
pub use router::{CapabilityBackend, CapabilityRouter, Invocation, InvocationResult, RouterError};
pub use task::{FarmTask, TaskError, TaskLedger, TaskState};
pub use trace::{
    infer_channel, PlanRecord, TaskRecord, ToolRecord, TraceError, TraceEvent, TraceStore,
    TraceToolStep, Trajectory,
};
#[cfg(feature = "wasm-runtime")]
pub use wasm::{WasmExecutor, WasmRuntimeError};
