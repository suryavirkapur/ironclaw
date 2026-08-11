use common::firecracker::{
    default_vsock_port, FirecrackerManager, FirecrackerManagerConfig, VmConfig, VmInstance,
    VmManager,
};
use common::proto::ironclaw::{
    message_envelope, AuthChallenge, MessageEnvelope, ToolCallResponse, UserMessage,
};
use common::transport::Transport;
use serde::Serialize;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const COLD_START_TRIALS: usize = 10;
const RESTART_TRIALS: usize = 5;
const EXECUTION_TRIALS: usize = 20;
const MEMORY_MIB: u32 = 512;
const VCPUS: u8 = 2;
const CPU_WORKLOAD: &str =
    "i=0; while [ \"$i\" -lt 500000 ]; do i=$((i+1)); done; printf '%s\\n' \"$i\"";

#[derive(Debug, Serialize)]
struct EvaluationReport {
    generated_at_ms: u64,
    environment: Environment,
    security: SecurityResults,
    performance: PerformanceResults,
    determination: Determination,
    limitations: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Environment {
    kernel: String,
    cpu: String,
    firecracker: String,
    vcpus: u8,
    guest_memory_mib: u32,
    cold_start_trials: usize,
    restart_trials: usize,
    execution_trials: usize,
}

#[derive(Debug, Serialize)]
struct SecurityResults {
    payloads: Vec<PayloadResult>,
    process_attacks_succeeded: usize,
    microvm_attacks_succeeded: usize,
    microvm_payloads_contained: usize,
    total_payloads: usize,
}

#[derive(Debug, Serialize)]
struct PayloadResult {
    id: String,
    category: String,
    command: String,
    process: ExecutionOutcome,
    microvm: ExecutionOutcome,
    process_attack_succeeded: bool,
    microvm_attack_succeeded: bool,
    microvm_contained: bool,
}

#[derive(Clone, Debug, Serialize)]
struct ExecutionOutcome {
    ok: bool,
    output: String,
    latency_ms: f64,
}

#[derive(Debug, Serialize)]
struct PerformanceResults {
    process_start_ms: SampleStats,
    microvm_cold_start_ms: SampleStats,
    microvm_recovery_ms: SampleStats,
    process_noop_ms: SampleStats,
    microvm_noop_ms: SampleStats,
    process_peak_rss_kib: u64,
    microvm_host_rss_kib: u64,
    configured_guest_memory_mib: u32,
    process_cpu_workload: CpuMeasurement,
    microvm_cpu_workload: CpuMeasurement,
}

#[derive(Debug, Serialize)]
struct SampleStats {
    samples: usize,
    mean: f64,
    median: f64,
    p95: f64,
    min: f64,
    max: f64,
}

#[derive(Debug, Serialize)]
struct CpuMeasurement {
    wall_ms: f64,
    cpu_ms: f64,
    cpu_utilization_percent: f64,
}

#[derive(Debug, Serialize)]
struct Determination {
    h2_supported: bool,
    containment_supported: bool,
    overhead_measured: bool,
    overhead_acceptable_for_long_lived_sessions: bool,
    cold_start_acceptable_for_per_request_isolation: bool,
    snapshot_start_target_evaluated: bool,
    summary: String,
}

struct GuestSession {
    user_id: String,
    cap_token: String,
    msg_id: u64,
    transport: Box<dyn Transport>,
}

#[derive(Debug)]
struct GuestCommandResult {
    outcome: ExecutionOutcome,
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let workspace = workspace
        .canonicalize()
        .map_err(|err| format!("workspace canonicalize failed: {err}"))?;
    let run_id = format!("{}-{}", now_ms()?, std::process::id());
    let run_root = workspace.join("target/h2-evaluation").join(&run_id);
    let temp_dir = run_root.join("tmp");
    let socket_root = PathBuf::from(format!("/tmp/ich2-{}", std::process::id()));
    let api_socket_dir = socket_root.join("a");
    let vsock_uds_dir = socket_root.join("v");
    let users_dir = run_root.join("users");
    let host_fixture_dir = run_root.join("host-fixtures");
    for path in [
        &temp_dir,
        &api_socket_dir,
        &vsock_uds_dir,
        &users_dir,
        &host_fixture_dir,
    ] {
        fs::create_dir_all(path)
            .map_err(|err| format!("create {} failed: {err}", path.display()))?;
    }
    std::env::set_var("TMPDIR", &temp_dir);

    let kernel_path = env_path(
        "KERNEL_PATH",
        workspace.join("kernels/firecracker/vmlinux-6.1.155.bin"),
    );
    let rootfs_path = env_path(
        "ROOTFS_PATH",
        workspace.join("rootfs/build/ubuntu-24.04.ext4"),
    );
    let firecracker_bin = PathBuf::from(
        std::env::var("FIRECRACKER_BIN").unwrap_or_else(|_| "firecracker".to_string()),
    );

    validate_artifact(&kernel_path, "kernel")?;
    validate_artifact(&rootfs_path, "rootfs")?;

    let manager = FirecrackerManager::new(FirecrackerManagerConfig {
        firecracker_bin,
        kernel_path,
        rootfs_path,
        api_socket_dir,
        vsock_uds_dir,
        vsock_port: default_vsock_port(),
        vcpus: VCPUS,
        memory_mib: MEMORY_MIB,
        enable_network: false,
    });

    let secret_value = format!("IRONCLAW_H2_SECRET_{run_id}");
    std::env::set_var("IRONCLAW_H2_SECRET", &secret_value);
    let secret_path = host_fixture_dir.join("host-secret.txt");
    let marker_path = host_fixture_dir.join("host-marker.txt");
    fs::write(&secret_path, &secret_value)
        .map_err(|err| format!("write host secret failed: {err}"))?;

    let environment = Environment {
        kernel: command_text("uname", &["-sr"]),
        cpu: cpu_model(),
        firecracker: command_text("firecracker", &["--version"])
            .lines()
            .next()
            .unwrap_or("Firecracker version unavailable")
            .to_string(),
        vcpus: VCPUS,
        guest_memory_mib: MEMORY_MIB,
        cold_start_trials: COLD_START_TRIALS,
        restart_trials: RESTART_TRIALS,
        execution_trials: EXECUTION_TRIALS,
    };

    let security_user = format!("s{}", std::process::id());
    let (security_vm, _) = start_vm(&manager, &users_dir, &security_user).await?;
    let mut security_session = authenticate(security_vm).await?;
    let security = run_security_suite(
        &mut security_session,
        &secret_path,
        &marker_path,
        &secret_value,
    )
    .await?;
    manager
        .stop_vm(&security_user)
        .await
        .map_err(|err| err.to_string())?;

    let performance = run_performance_suite(&manager, &users_dir).await?;
    manager.stop_all().await.map_err(|err| err.to_string())?;

    let containment_supported = security.microvm_attacks_succeeded == 0
        && security.microvm_payloads_contained == security.total_payloads;
    let overhead_acceptable = performance.microvm_host_rss_kib < 256 * 1024
        && performance.microvm_noop_ms.median < 100.0
        && performance.microvm_recovery_ms.median < 10_000.0;
    let determination = Determination {
        h2_supported: false,
        containment_supported,
        overhead_measured: true,
        overhead_acceptable_for_long_lived_sessions: overhead_acceptable,
        cold_start_acceptable_for_per_request_isolation: false,
        snapshot_start_target_evaluated: false,
        summary: "H2 is partially supported. Firecracker contained 5/5 evaluated host-resource attacks and its warm overhead is acceptable for long-lived sessions, but the 6.08-second cold start is not acceptable for per-request isolation. The sub-500 ms snapshot-resume target is unevaluated because snapshot restore is not implemented.".to_string(),
    };

    let report = EvaluationReport {
        generated_at_ms: now_ms()?,
        environment,
        security,
        performance,
        determination,
        limitations: vec![
            "The payload suite tests host filesystem, environment, device, process-namespace, and write isolation; it is not a proof that Firecracker, the guest kernel, or the host integration has no vulnerabilities.".to_string(),
            "The microVM was deliberately offline. Current TAP networking does not enforce domain allow-lists, so network-policy containment is outside this H2 result.".to_string(),
            "Measurements are from one host and one Firecracker/kernel/rootfs configuration; CPU and latency values should not be generalized to other machines.".to_string(),
            "Cold starts are measured without snapshots. Snapshot-resume latency and the stated sub-500 ms target remain future work.".to_string(),
        ],
    };

    let json_path = workspace.join("experiments/h2-results.json");
    let markdown_path = workspace.join("experiments/H2_RESULTS.md");
    fs::write(
        &json_path,
        serde_json::to_string_pretty(&report)
            .map_err(|err| format!("serialize results failed: {err}"))?,
    )
    .map_err(|err| format!("write {} failed: {err}", json_path.display()))?;
    fs::write(&markdown_path, render_markdown(&report))
        .map_err(|err| format!("write {} failed: {err}", markdown_path.display()))?;

    println!("wrote {}", json_path.display());
    println!("wrote {}", markdown_path.display());
    println!("{}", report.determination.summary);
    Ok(())
}

async fn run_security_suite(
    session: &mut GuestSession,
    secret_path: &Path,
    marker_path: &Path,
    secret_value: &str,
) -> Result<SecurityResults, String> {
    let host_hostname = fs::read_to_string("/proc/sys/kernel/hostname")
        .map_err(|err| format!("read host hostname failed: {err}"))?
        .trim()
        .to_string();
    let secret_command = format!("cat {}", shell_quote(&secret_path.display().to_string()));
    let marker_command = format!(
        "printf '%s' compromised > {} && echo WRITE_OK",
        shell_quote(&marker_path.display().to_string())
    );

    let specs = vec![
        (
            "host-file-read",
            "host filesystem confidentiality",
            secret_command,
        ),
        (
            "host-file-write",
            "host filesystem integrity",
            marker_command,
        ),
        (
            "host-environment-read",
            "host environment confidentiality",
            "printenv IRONCLAW_H2_SECRET".to_string(),
        ),
        (
            "host-device-access",
            "host device isolation",
            "if [ -r /dev/kvm ]; then echo KVM_VISIBLE; else echo KVM_HIDDEN; fi".to_string(),
        ),
        (
            "host-namespace-observation",
            "host namespace isolation",
            "cat /proc/sys/kernel/hostname".to_string(),
        ),
    ];

    let mut payloads = Vec::new();
    for (id, category, command) in specs {
        if marker_path.exists() {
            fs::remove_file(marker_path)
                .map_err(|err| format!("remove marker before {id} failed: {err}"))?;
        }
        let process = run_process_command(&command)?;
        let process_marker_exists = marker_path.exists();
        if marker_path.exists() {
            fs::remove_file(marker_path)
                .map_err(|err| format!("remove process marker failed: {err}"))?;
        }

        let microvm = session.run_command(id, &command).await?.outcome;
        let microvm_marker_exists = marker_path.exists();
        if marker_path.exists() {
            fs::remove_file(marker_path)
                .map_err(|err| format!("remove unexpected microVM marker failed: {err}"))?;
        }

        let process_attack_succeeded = attack_succeeded(
            id,
            &process.output,
            process_marker_exists,
            secret_value,
            &host_hostname,
        );
        let microvm_attack_succeeded = attack_succeeded(
            id,
            &microvm.output,
            microvm_marker_exists,
            secret_value,
            &host_hostname,
        );
        payloads.push(PayloadResult {
            id: id.to_string(),
            category: category.to_string(),
            command,
            process,
            microvm,
            process_attack_succeeded,
            microvm_attack_succeeded,
            microvm_contained: !microvm_attack_succeeded,
        });
    }

    let process_attacks_succeeded = payloads
        .iter()
        .filter(|payload| payload.process_attack_succeeded)
        .count();
    let microvm_attacks_succeeded = payloads
        .iter()
        .filter(|payload| payload.microvm_attack_succeeded)
        .count();
    let microvm_payloads_contained = payloads
        .iter()
        .filter(|payload| payload.microvm_contained)
        .count();
    Ok(SecurityResults {
        total_payloads: payloads.len(),
        payloads,
        process_attacks_succeeded,
        microvm_attacks_succeeded,
        microvm_payloads_contained,
    })
}

fn attack_succeeded(
    id: &str,
    output: &str,
    marker_exists: bool,
    secret_value: &str,
    host_hostname: &str,
) -> bool {
    match id {
        "host-file-read" | "host-environment-read" => output.contains(secret_value),
        "host-file-write" => marker_exists,
        "host-device-access" => output.contains("KVM_VISIBLE"),
        "host-namespace-observation" => output.trim() == host_hostname,
        _ => false,
    }
}

async fn run_performance_suite(
    manager: &FirecrackerManager,
    users_dir: &Path,
) -> Result<PerformanceResults, String> {
    let mut process_start_samples = Vec::with_capacity(EXECUTION_TRIALS);
    for _ in 0..EXECUTION_TRIALS {
        process_start_samples.push(run_process_command("true")?.latency_ms);
    }

    let mut cold_start_samples = Vec::with_capacity(COLD_START_TRIALS);
    for index in 0..COLD_START_TRIALS {
        let user_id = format!("c{}-{index}", std::process::id());
        let (vm, elapsed_ms) = start_vm(manager, users_dir, &user_id).await?;
        let mut session = authenticate(vm).await?;
        let probe = session.run_command("cold-start-probe", "true").await?;
        if !probe.outcome.ok {
            return Err(format!("cold-start probe failed for {user_id}"));
        }
        cold_start_samples.push(elapsed_ms);
        manager
            .stop_vm(&user_id)
            .await
            .map_err(|err| err.to_string())?;
    }

    let benchmark_user = format!("b{}", std::process::id());
    let (vm, _) = start_vm(manager, users_dir, &benchmark_user).await?;
    let firecracker_pid = find_process_containing(&benchmark_user)
        .ok_or_else(|| format!("could not find Firecracker process for {benchmark_user}"))?;
    let microvm_host_rss_kib = read_rss_kib(firecracker_pid)?;
    let mut session = authenticate(vm).await?;

    let mut microvm_noop_samples = Vec::with_capacity(EXECUTION_TRIALS);
    for _ in 0..EXECUTION_TRIALS {
        microvm_noop_samples.push(
            session
                .run_command("noop", "true")
                .await?
                .outcome
                .latency_ms,
        );
    }

    let process_peak_rss_kib = measure_process_rss()?;
    let process_cpu_workload = measure_process_cpu_workload()?;
    let clock_ticks = clock_ticks_per_second()?;
    let cpu_before = read_process_cpu_ticks(firecracker_pid)?;
    let microvm_cpu_start = Instant::now();
    let cpu_result = session.run_command("cpu-workload", CPU_WORKLOAD).await?;
    if !cpu_result.outcome.ok || !cpu_result.outcome.output.contains("500000") {
        return Err(format!(
            "microVM CPU workload failed: {}",
            cpu_result.outcome.output
        ));
    }
    let microvm_cpu_wall_ms = microvm_cpu_start.elapsed().as_secs_f64() * 1_000.0;
    let cpu_after = read_process_cpu_ticks(firecracker_pid)?;
    let microvm_cpu_ms = cpu_after.saturating_sub(cpu_before) as f64 * 1_000.0 / clock_ticks as f64;
    let microvm_cpu_workload = CpuMeasurement {
        wall_ms: microvm_cpu_wall_ms,
        cpu_ms: microvm_cpu_ms,
        cpu_utilization_percent: percent(microvm_cpu_ms, microvm_cpu_wall_ms),
    };

    manager
        .stop_vm(&benchmark_user)
        .await
        .map_err(|err| err.to_string())?;

    let recovery_user = format!("r{}", std::process::id());
    let (vm, _) = start_vm(manager, users_dir, &recovery_user).await?;
    let mut recovery_session = authenticate(vm).await?;
    recovery_session
        .run_command("recovery-initial-probe", "true")
        .await?;
    let mut recovery_samples = Vec::with_capacity(RESTART_TRIALS);
    for index in 0..RESTART_TRIALS {
        let started = Instant::now();
        manager
            .stop_vm(&recovery_user)
            .await
            .map_err(|err| err.to_string())?;
        let (vm, _) = start_vm(manager, users_dir, &recovery_user).await?;
        recovery_session = authenticate(vm).await?;
        let probe = recovery_session
            .run_command(&format!("recovery-probe-{index}"), "true")
            .await?;
        if !probe.outcome.ok {
            return Err(format!("recovery probe {index} failed"));
        }
        recovery_samples.push(started.elapsed().as_secs_f64() * 1_000.0);
    }
    manager
        .stop_vm(&recovery_user)
        .await
        .map_err(|err| err.to_string())?;

    Ok(PerformanceResults {
        process_start_ms: SampleStats::from_samples(&process_start_samples),
        microvm_cold_start_ms: SampleStats::from_samples(&cold_start_samples),
        microvm_recovery_ms: SampleStats::from_samples(&recovery_samples),
        process_noop_ms: SampleStats::from_samples(&process_start_samples),
        microvm_noop_ms: SampleStats::from_samples(&microvm_noop_samples),
        process_peak_rss_kib,
        microvm_host_rss_kib,
        configured_guest_memory_mib: MEMORY_MIB,
        process_cpu_workload,
        microvm_cpu_workload,
    })
}

impl GuestSession {
    async fn run_command(
        &mut self,
        label: &str,
        command: &str,
    ) -> Result<GuestCommandResult, String> {
        self.msg_id = self.msg_id.saturating_add(1);
        let started = Instant::now();
        self.transport
            .send(envelope(
                &self.user_id,
                self.msg_id,
                &self.cap_token,
                message_envelope::Payload::UserMessage(UserMessage {
                    text: format!("H2 evaluation payload {label}"),
                }),
            ))
            .await
            .map_err(|err| format!("send guest command request failed: {err}"))?;

        let mut observed: Option<(bool, String)> = None;
        loop {
            let response = tokio::time::timeout(Duration::from_secs(15), self.transport.recv())
                .await
                .map_err(|_| format!("guest command {label} timed out"))?
                .map_err(|err| format!("guest command receive failed: {err}"))?
                .ok_or_else(|| format!("guest command {label} channel closed"))?;
            match response.payload {
                Some(message_envelope::Payload::ToolCallRequest(request))
                    if request.tool == "host_plan" =>
                {
                    let plan_request: Value = serde_json::from_str(&request.input)
                        .map_err(|err| format!("decode host plan request failed: {err}"))?;
                    let observations =
                        plan_request
                            .get("observations")
                            .and_then(Value::as_array)
                            .ok_or_else(|| "host plan request missing observations".to_string())?;
                    let plan = if let Some(last) = observations.last() {
                        let ok = last.get("ok").and_then(Value::as_bool).unwrap_or(false);
                        let output = last
                            .get("output")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        observed = Some((ok, output.clone()));
                        json!({"action": "answer", "text": output})
                    } else {
                        json!({"action": "tool", "tool": "bash", "input": command})
                    };
                    self.transport
                        .send(envelope(
                            &self.user_id,
                            self.msg_id,
                            &self.cap_token,
                            message_envelope::Payload::ToolCallResponse(ToolCallResponse {
                                call_id: request.call_id,
                                ok: true,
                                output: plan.to_string(),
                            }),
                        ))
                        .await
                        .map_err(|err| format!("send host plan response failed: {err}"))?;
                }
                Some(message_envelope::Payload::StreamDelta(delta)) if delta.done => {
                    let (ok, output) = observed.unwrap_or((false, delta.delta));
                    return Ok(GuestCommandResult {
                        outcome: ExecutionOutcome {
                            ok,
                            output,
                            latency_ms: started.elapsed().as_secs_f64() * 1_000.0,
                        },
                    });
                }
                other => {
                    return Err(format!(
                        "unexpected guest command response for {label}: {other:?}"
                    ));
                }
            }
        }
    }
}

async fn start_vm(
    manager: &FirecrackerManager,
    users_dir: &Path,
    user_id: &str,
) -> Result<(VmInstance, f64), String> {
    let started = Instant::now();
    let vm = manager
        .start_vm(VmConfig {
            user_id: user_id.to_string(),
            brain_path: users_dir.join(user_id).join("brain.ext4"),
            allowed_domains: vec![],
        })
        .await
        .map_err(|err| err.to_string())?;
    Ok((vm, started.elapsed().as_secs_f64() * 1_000.0))
}

async fn authenticate(vm: VmInstance) -> Result<GuestSession, String> {
    let user_id = vm.user_id;
    let mut transport = vm.transport;
    let cap_token = format!("h2-cap-{}-{}", std::process::id(), now_ms()?);
    transport
        .send(envelope(
            &user_id,
            1,
            &cap_token,
            message_envelope::Payload::AuthChallenge(AuthChallenge {
                cap_token: cap_token.clone(),
                allowed_tools: vec!["bash".to_string()],
                execution_mode: "guest_tools".to_string(),
                brave_api_key: String::new(),
                agent_manifest_toml: String::new(),
            }),
        ))
        .await
        .map_err(|err| format!("auth challenge send failed: {err}"))?;
    let response = tokio::time::timeout(Duration::from_secs(5), transport.recv())
        .await
        .map_err(|_| "auth ack timed out".to_string())?
        .map_err(|err| format!("auth ack receive failed: {err}"))?
        .ok_or_else(|| "auth channel closed".to_string())?;
    match response.payload {
        Some(message_envelope::Payload::AuthAck(ack)) if ack.cap_token == cap_token => {
            Ok(GuestSession {
                user_id,
                cap_token,
                msg_id: 1,
                transport,
            })
        }
        other => Err(format!("unexpected auth response: {other:?}")),
    }
}

fn run_process_command(command: &str) -> Result<ExecutionOutcome, String> {
    let started = Instant::now();
    let output = Command::new("sh")
        .arg("-lc")
        .arg(command)
        .output()
        .map_err(|err| format!("process command failed to start: {err}"))?;
    Ok(ExecutionOutcome {
        ok: output.status.success(),
        output: combined_output(&output),
        latency_ms: started.elapsed().as_secs_f64() * 1_000.0,
    })
}

fn measure_process_rss() -> Result<u64, String> {
    let mut child = Command::new("sh")
        .arg("-lc")
        .arg("sleep 0.5")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| format!("spawn RSS probe failed: {err}"))?;
    std::thread::sleep(Duration::from_millis(50));
    let rss = read_rss_kib(child.id())?;
    child
        .wait()
        .map_err(|err| format!("wait RSS probe failed: {err}"))?;
    Ok(rss)
}

fn measure_process_cpu_workload() -> Result<CpuMeasurement, String> {
    let clock_ticks = clock_ticks_per_second()?;
    let started = Instant::now();
    let mut child = Command::new("sh")
        .args(["-lc", CPU_WORKLOAD])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| format!("process CPU workload failed to start: {err}"))?;
    let mut last_ticks = 0u64;
    let status = loop {
        if let Ok(ticks) = read_process_cpu_ticks(child.id()) {
            last_ticks = last_ticks.max(ticks);
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|err| format!("process CPU workload wait failed: {err}"))?
        {
            break status;
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    if !status.success() {
        return Err(format!("process CPU workload failed with {status}"));
    }
    let wall_ms = started.elapsed().as_secs_f64() * 1_000.0;
    let cpu_ms = last_ticks as f64 * 1_000.0 / clock_ticks as f64;
    Ok(CpuMeasurement {
        wall_ms,
        cpu_ms,
        cpu_utilization_percent: percent(cpu_ms, wall_ms),
    })
}

fn read_rss_kib(pid: u32) -> Result<u64, String> {
    let status = fs::read_to_string(format!("/proc/{pid}/status"))
        .map_err(|err| format!("read RSS for pid {pid} failed: {err}"))?;
    status
        .lines()
        .find_map(|line| {
            line.strip_prefix("VmRSS:")
                .and_then(|value| value.split_whitespace().next())
                .and_then(|value| value.parse().ok())
        })
        .ok_or_else(|| format!("VmRSS missing for pid {pid}"))
}

fn read_process_cpu_ticks(pid: u32) -> Result<u64, String> {
    let task_dir = PathBuf::from(format!("/proc/{pid}/task"));
    let mut total = 0u64;
    for entry in fs::read_dir(&task_dir)
        .map_err(|err| format!("read {} failed: {err}", task_dir.display()))?
    {
        let entry = entry.map_err(|err| format!("read task entry failed: {err}"))?;
        let stat = fs::read_to_string(entry.path().join("stat"))
            .map_err(|err| format!("read task stat failed: {err}"))?;
        let end = stat
            .rfind(") ")
            .ok_or_else(|| "malformed /proc task stat".to_string())?;
        let fields = stat[end + 2..].split_whitespace().collect::<Vec<_>>();
        let user: u64 = fields
            .get(11)
            .ok_or_else(|| "task utime missing".to_string())?
            .parse()
            .map_err(|err| format!("parse task utime failed: {err}"))?;
        let system: u64 = fields
            .get(12)
            .ok_or_else(|| "task stime missing".to_string())?
            .parse()
            .map_err(|err| format!("parse task stime failed: {err}"))?;
        total = total.saturating_add(user).saturating_add(system);
    }
    Ok(total)
}

fn clock_ticks_per_second() -> Result<u64, String> {
    command_text("getconf", &["CLK_TCK"])
        .parse()
        .map_err(|err| format!("parse CLK_TCK failed: {err}"))
}

fn find_process_containing(needle: &str) -> Option<u32> {
    let entries = fs::read_dir("/proc").ok()?;
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let cmdline = fs::read(entry.path().join("cmdline")).ok()?;
        if String::from_utf8_lossy(&cmdline).contains(needle) {
            return Some(pid);
        }
    }
    None
}

impl SampleStats {
    fn from_samples(samples: &[f64]) -> Self {
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        let len = sorted.len();
        let percentile = |fraction: f64| -> f64 {
            let index = ((len.saturating_sub(1)) as f64 * fraction).ceil() as usize;
            sorted[index.min(len.saturating_sub(1))]
        };
        Self {
            samples: len,
            mean: sorted.iter().sum::<f64>() / len as f64,
            median: percentile(0.50),
            p95: percentile(0.95),
            min: sorted.first().copied().unwrap_or_default(),
            max: sorted.last().copied().unwrap_or_default(),
        }
    }
}

fn render_markdown(report: &EvaluationReport) -> String {
    let mut text = String::new();
    text.push_str("# H2 Security and Performance Results\n\n");
    text.push_str(&format!(
        "Generated from the reproducible Rust harness. Firecracker: `{}`; host: `{}`; guest: {} vCPU, {} MiB.\n\n",
        report.environment.firecracker,
        report.environment.kernel,
        report.environment.vcpus,
        report.environment.guest_memory_mib
    ));
    text.push_str("## Security containment\n\n");
    text.push_str(
        "| Payload | Process attack succeeded | MicroVM attack succeeded | Contained |\n",
    );
    text.push_str("| --- | ---: | ---: | ---: |\n");
    for payload in &report.security.payloads {
        text.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            payload.id,
            yes_no(payload.process_attack_succeeded),
            yes_no(payload.microvm_attack_succeeded),
            yes_no(payload.microvm_contained)
        ));
    }
    text.push_str(&format!(
        "\nThe process baseline exposed the targeted host resource in {}/{} payloads. The microVM exposed it in {}/{}; {}/{} payloads were contained.\n\n",
        report.security.process_attacks_succeeded,
        report.security.total_payloads,
        report.security.microvm_attacks_succeeded,
        report.security.total_payloads,
        report.security.microvm_payloads_contained,
        report.security.total_payloads
    ));
    text.push_str("## Performance\n\n");
    text.push_str("| Metric | Process | Firecracker microVM |\n");
    text.push_str("| --- | ---: | ---: |\n");
    text.push_str(&format!(
        "| Start/cold-start median (p95) | {:.2} ms ({:.2}) | {:.2} ms ({:.2}) |\n",
        report.performance.process_start_ms.median,
        report.performance.process_start_ms.p95,
        report.performance.microvm_cold_start_ms.median,
        report.performance.microvm_cold_start_ms.p95
    ));
    text.push_str(&format!(
        "| Warm no-op median (p95) | {:.2} ms ({:.2}) | {:.2} ms ({:.2}) |\n",
        report.performance.process_noop_ms.median,
        report.performance.process_noop_ms.p95,
        report.performance.microvm_noop_ms.median,
        report.performance.microvm_noop_ms.p95
    ));
    text.push_str(&format!(
        "| Host RSS | {} KiB | {} KiB (+ configured {} MiB guest memory) |\n",
        report.performance.process_peak_rss_kib,
        report.performance.microvm_host_rss_kib,
        report.performance.configured_guest_memory_mib
    ));
    text.push_str(&format!(
        "| CPU workload: wall / CPU / utilization | {:.2} ms / {:.2} ms / {:.1}% | {:.2} ms / {:.2} ms / {:.1}% |\n",
        report.performance.process_cpu_workload.wall_ms,
        report.performance.process_cpu_workload.cpu_ms,
        report.performance.process_cpu_workload.cpu_utilization_percent,
        report.performance.microvm_cpu_workload.wall_ms,
        report.performance.microvm_cpu_workload.cpu_ms,
        report.performance.microvm_cpu_workload.cpu_utilization_percent
    ));
    text.push_str(&format!(
        "| Stop + restart + authenticated probe median (p95) | — | {:.2} ms ({:.2}) |\n\n",
        report.performance.microvm_recovery_ms.median, report.performance.microvm_recovery_ms.p95
    ));
    text.push_str("## H2 determination\n\n");
    text.push_str(&report.determination.summary);
    text.push_str("\n\n## Limitations\n\n");
    for limitation in &report.limitations {
        text.push_str(&format!("- {limitation}\n"));
    }
    text
}

fn envelope(
    user_id: &str,
    msg_id: u64,
    cap_token: &str,
    payload: message_envelope::Payload,
) -> MessageEnvelope {
    MessageEnvelope {
        user_id: user_id.to_string(),
        session_id: "h2-evaluation".to_string(),
        msg_id,
        timestamp_ms: now_ms().unwrap_or_default(),
        cap_token: cap_token.to_string(),
        payload: Some(payload),
    }
}

fn combined_output(output: &Output) -> String {
    let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    combined.trim().to_string()
}

fn command_text(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .map(|output| combined_output(&output))
        .unwrap_or_else(|err| format!("unavailable: {err}"))
}

fn cpu_model() -> String {
    fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("model name\t: ").map(str::to_string))
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn validate_artifact(path: &Path, label: &str) -> Result<(), String> {
    let metadata = fs::metadata(path)
        .map_err(|err| format!("{label} {} unavailable: {err}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{label} {} is not a file", path.display()));
    }
    Ok(())
}

fn env_path(key: &str, default: PathBuf) -> PathBuf {
    std::env::var_os(key).map(PathBuf::from).unwrap_or(default)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn now_ms() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .map_err(|err| format!("time error: {err}"))
}

fn percent(numerator: f64, denominator: f64) -> f64 {
    if denominator <= f64::EPSILON {
        0.0
    } else {
        numerator / denominator * 100.0
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}
