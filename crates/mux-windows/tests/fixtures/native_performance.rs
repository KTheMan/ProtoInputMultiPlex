//! Opt-in backend comparison. Measurements stay in process/stdout, not report files.
//! This measures submission-to-query-observed consumption and backend shutdown.
//! It does not qualify worker dispatch, router release, exit or supervisor commit
//! latency as an end-to-end production Stop measurement.
use super::*;

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct ControllerResources {
    working_set_bytes: u64,
    private_bytes: u64,
    handles: u32,
}

// PROCESS_MEMORY_COUNTERS_EX, shared by the supported Windows pointer widths.
// https://learn.microsoft.com/windows/win32/api/psapi/ns-psapi-process_memory_counters_ex
#[repr(C)]
#[derive(Default)]
struct MemoryCounters {
    bytes: u32,
    page_faults: u32,
    peak_working_set: usize,
    working_set: usize,
    peak_paged_pool: usize,
    paged_pool: usize,
    peak_nonpaged_pool: usize,
    nonpaged_pool: usize,
    pagefile: usize,
    peak_pagefile: usize,
    private_usage: usize,
}

const _: () = assert!(size_of::<MemoryCounters>() == if usize::BITS == 64 { 80 } else { 44 });

#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "K32GetProcessMemoryInfo"]
    fn process_memory_info(
        process: windows_sys::Win32::Foundation::HANDLE,
        counters: *mut MemoryCounters,
        bytes: u32,
    ) -> i32;
}

pub(super) fn resources() -> Result<ControllerResources> {
    let bytes = size_of::<MemoryCounters>() as u32;
    let mut counters = MemoryCounters {
        bytes,
        ..MemoryCounters::default()
    };
    let process = unsafe { GetCurrentProcess() };
    ensure!(
        unsafe { process_memory_info(process, &mut counters, bytes) } != 0,
        "read owned controller memory usage: {}",
        std::io::Error::last_os_error()
    );
    let mut handles = 0;
    ensure!(
        unsafe { GetProcessHandleCount(process, &mut handles) } != 0,
        "read owned controller handle count: {}",
        std::io::Error::last_os_error()
    );
    Ok(ControllerResources {
        working_set_bytes: counters.working_set as u64,
        private_bytes: counters.private_usage as u64,
        handles,
    })
}

pub(super) fn measure_input(
    session: &mux_windows::protoinput::Session,
    seats: &[ResolvedSeat],
    targets: &[SelectedTarget],
) -> Result<Vec<u64>> {
    let mut samples = Vec::new();
    for (index, target) in targets.iter().enumerate() {
        let keyboard = seats[index].keyboard.context("benchmark keyboard")?;
        let down = query(target, 6)?;
        let up = query(target, 7)?;
        // Warm the transport and hook path before retaining 100 edge samples.
        // B uses separate real WndProc counters so the functional A assertions
        // remain independently checked by the supervisor after each cycle.
        for iteration in 0..60 {
            for (released, counter, expected) in [
                (false, 6, down + iteration + 1),
                (true, 7, up + iteration + 1),
            ] {
                let start = Instant::now();
                session.submit_to(
                    index,
                    target.hwnd,
                    Packet::key_scanned(keyboard, 0x42, 0x30, 0, released),
                )?;
                loop {
                    if query(target, counter)? == expected {
                        break;
                    }
                    ensure!(
                        start.elapsed() < POLL_LIMIT,
                        "benchmark edge was not consumed"
                    );
                    std::thread::yield_now();
                }
                if iteration >= 10 {
                    samples.push(start.elapsed().as_nanos().try_into()?);
                }
            }
        }
    }
    Ok(samples)
}

fn p95(samples: &mut [u64]) -> Result<u64> {
    ensure!(!samples.is_empty(), "missing performance samples");
    samples.sort_unstable();
    Ok(samples[(samples.len() * 95).div_ceil(100) - 1])
}

#[test]
#[ignore = "opt-in native/Rust input and backend shutdown comparison on owned fixtures"]
fn rust_virtual_native_backend_performance_comparison() -> Result<()> {
    let runtime = PathBuf::from(std::env::var_os(RUNTIME).context("native runtime opt-in")?)
        .canonicalize()?;
    for count in [1, 2] {
        let mut rust = (0..count)
            .map(|_| spawn_target())
            .collect::<Result<Vec<_>>>()?;
        let mut native = (0..count)
            .map(|_| spawn_target())
            .collect::<Result<Vec<_>>>()?;
        let mut rust_input = Vec::new();
        let mut native_input = Vec::new();
        let mut rust_stop = Vec::new();
        let mut native_stop = Vec::new();
        let mut rust_resources = Vec::new();
        let mut native_resources = Vec::new();
        for cycle in 0..30 {
            let order = if cycle % 2 == 0 {
                ["rust", "native"]
            } else {
                ["native", "rust"]
            };
            for mode in order {
                let (fixtures, input, stop) = if mode == "rust" {
                    (&mut rust, &mut rust_input, &mut rust_stop)
                } else {
                    (&mut native, &mut native_input, &mut native_stop)
                };
                let observed = run_cycle(&runtime, mode, fixtures, "performance")?;
                ensure!(
                    observed.messages == vec![expected_evidence(); count],
                    "benchmark functional regression"
                );
                let metrics = observed
                    .metrics
                    .context("missing controller measurements")?;
                ensure!(
                    metrics.input_ns.len() == count * 100,
                    "missing consumed-edge samples"
                );
                input.extend(metrics.input_ns);
                stop.push(metrics.backend_shutdown_ns);
                if mode == "rust" {
                    rust_resources.push(metrics.resources);
                } else {
                    native_resources.push(metrics.resources);
                }
            }
        }
        let rust_input_p95 = p95(&mut rust_input)?;
        let native_input_p95 = p95(&mut native_input)?;
        let rust_stop_p95 = p95(&mut rust_stop)?;
        let native_stop_p95 = p95(&mut native_stop)?;
        // Limits explicitly accepted by the user before measuring/judging.
        let input_limit = native_input_p95.saturating_add(1_000_000.max(native_input_p95 / 4));
        let stop_limit = native_stop_p95.saturating_add(250_000_000);
        let resource_p95 = |samples: &[ControllerResources]| -> Result<_> {
            Ok((
                p95(&mut samples
                    .iter()
                    .map(|sample| sample.private_bytes)
                    .collect::<Vec<_>>())?,
                p95(&mut samples
                    .iter()
                    .map(|sample| sample.working_set_bytes)
                    .collect::<Vec<_>>())?,
                p95(&mut samples
                    .iter()
                    .map(|sample| u64::from(sample.handles))
                    .collect::<Vec<_>>())?,
            ))
        };
        let rust_usage = resource_p95(&rust_resources)?;
        let native_usage = resource_p95(&native_resources)?;
        println!(
            "MIGRATION_BACKEND_PERFORMANCE targets={count} controller={}bit target={}bit input_samples={} shutdown_samples={} p95_submission_to_observed_consumption_ns rust={rust_input_p95} native={native_input_p95} limit={input_limit} p95_backend_shutdown_ns rust={rust_stop_p95} native={native_stop_p95} limit={stop_limit}; production_Stop_gate_remains_open",
            usize::BITS,
            if std::env::var_os(TARGET_FIXTURE).is_some() {
                32
            } else {
                usize::BITS
            },
            rust_input.len(),
            rust_stop.len()
        );
        // Observe live controller costs; no unagreed resource threshold is used
        // to claim rollout acceptance or a general leak/performance result.
        println!(
            "MIGRATION_CONTROLLER_RESOURCES targets={count} controller={}bit samples_per_backend={} p95_private_bytes rust={} native={} p95_working_set_bytes rust={} native={} p95_handles rust={} native={}; observed_while_routing_before_shutdown",
            usize::BITS,
            rust_resources.len(),
            rust_usage.0,
            native_usage.0,
            rust_usage.1,
            native_usage.1,
            rust_usage.2,
            native_usage.2
        );
        ensure!(
            rust_input_p95 <= input_limit,
            "Rust p95 input regression exceeds agreed limit"
        );
        ensure!(
            rust_stop_p95 <= stop_limit,
            "Rust p95 backend shutdown regression exceeds agreed Stop budget"
        );
    }
    Ok(())
}
