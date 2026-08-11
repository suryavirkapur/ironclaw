# H2 Security and Performance Results

Generated from the reproducible Rust harness. Firecracker: `Firecracker v1.14.0`; host: `Linux 7.0.12-arch1-1`; guest: 2 vCPU, 512 MiB.

## Security containment

| Payload | Process attack succeeded | MicroVM attack succeeded | Contained |
| --- | ---: | ---: | ---: |
| host-file-read | yes | no | yes |
| host-file-write | yes | no | yes |
| host-environment-read | yes | no | yes |
| host-device-access | yes | no | yes |
| host-namespace-observation | yes | no | yes |

The process baseline exposed the targeted host resource in 5/5 payloads. The microVM exposed it in 0/5; 5/5 payloads were contained.

## Performance

| Metric | Process | Firecracker microVM |
| --- | ---: | ---: |
| Start/cold-start median (p95) | 36.45 ms (94.00) | 6083.67 ms (6102.59) |
| Warm no-op median (p95) | 36.45 ms (94.00) | 1.74 ms (3.68) |
| Host RSS | 4108 KiB | 72672 KiB (+ configured 512 MiB guest memory) |
| CPU workload: wall / CPU / utilization | 922.27 ms / 850.00 ms / 92.2% | 321.31 ms / 320.00 ms / 99.6% |
| Stop + restart + authenticated probe median (p95) | — | 6622.87 ms (6632.47) |

## H2 determination

H2 is partially supported. Firecracker contained 5/5 evaluated host-resource attacks and its warm overhead is acceptable for long-lived sessions, but the 6.08-second cold start is not acceptable for per-request isolation. The sub-500 ms snapshot-resume target is unevaluated because snapshot restore is not implemented.

## Limitations

- The payload suite tests host filesystem, environment, device, process-namespace, and write isolation; it is not a proof that Firecracker, the guest kernel, or the host integration has no vulnerabilities.
- The microVM was deliberately offline. Current TAP networking does not enforce domain allow-lists, so network-policy containment is outside this H2 result.
- Measurements are from one host and one Firecracker/kernel/rootfs configuration; CPU and latency values should not be generalized to other machines.
- Cold starts are measured without snapshots. Snapshot-resume latency and the stated sub-500 ms target remain future work.
