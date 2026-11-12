//! Passive native observations. Failed reads are unknown, never guessed zero.
use std::time::Instant;

#[derive(Clone, Copy)]
pub(super) struct Process {
    pub start: u64,
    pub cpu_seconds: f64,
    pub memory: u64,
}
#[derive(Default)]
pub(super) struct Host {
    pub cpu: Option<(u64, u64)>, // busy, total native ticks
    pub memory_used: Option<u64>,
    pub memory_total: Option<u64>,
}
#[derive(Default)]
pub(super) struct CpuDelta {
    previous: Option<(u64, u64, f64, Instant)>,
}
impl CpuDelta {
    pub fn observe(&mut self, pid: u32, value: Option<Process>, now: Instant) -> Option<f64> {
        let Some(value) = value else {
            self.previous = None;
            return None;
        };
        let old = self
            .previous
            .replace((u64::from(pid), value.start, value.cpu_seconds, now))?;
        if old.0 != u64::from(pid) || old.1 != value.start {
            return None;
        }
        let elapsed = now.checked_duration_since(old.3)?.as_secs_f64();
        let delta = value.cpu_seconds - old.2;
        (elapsed > 0.0 && delta >= 0.0)
            .then(|| {
                total_cpu_percent(
                    delta,
                    elapsed,
                    std::thread::available_parallelism().ok()?.get(),
                )
            })
            .flatten()
            .filter(|v| v.is_finite())
    }
}

// Match the host CPU scale: all available logical processors together are 100%.
fn total_cpu_percent(seconds: f64, elapsed: f64, processors: usize) -> Option<f64> {
    if processors == 0 || elapsed <= 0.0 || seconds < 0.0 {
        return None;
    }
    let percent = seconds / elapsed / processors as f64 * 100.0;
    percent.is_finite().then(|| percent.clamp(0.0, 100.0))
}
#[cfg(windows)]
#[allow(unsafe_code)]
pub(super) fn process(pid: u32, expected_start: Option<u64>) -> Option<Process> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, FILETIME},
        System::{
            ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Threading::{
                GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_INFORMATION,
                PROCESS_VM_READ,
            },
        },
    };
    // SAFETY: OpenProcess accepts a numeric PID and requests only observation rights.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let result = (|| {
        // SAFETY: FILETIME consists of two u32 fields and admits a zero representation.
        let mut created: FILETIME = unsafe { std::mem::zeroed() };
        let mut exited = created;
        let mut kernel = created;
        let mut user = created;
        let mut code = 0;
        // SAFETY: handle remains open; all output pointers target valid FILETIMEs/u32.
        if unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) }
            == 0
            || unsafe { GetExitCodeProcess(handle, &mut code) } == 0
            || code != 259
        {
            return None;
        }
        let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
        let start = ticks(created);
        if expected_start.is_some_and(|expected| expected != start) {
            return None;
        }
        // SAFETY: zero is a valid initial counter structure, and cb describes its exact size.
        let mut memory: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
        memory.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        // SAFETY: valid open process handle and writable structure of the supplied size.
        if unsafe { K32GetProcessMemoryInfo(handle, &mut memory, memory.cb) } == 0 {
            return None;
        }
        Some(Process {
            start,
            cpu_seconds: ticks(kernel).checked_add(ticks(user))? as f64 / 10_000_000.0,
            memory: memory.WorkingSetSize as u64,
        })
    })();
    // SAFETY: this function exclusively owns the successfully opened handle.
    unsafe {
        CloseHandle(handle);
    }
    result
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub(super) fn host() -> Host {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::{
            SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX},
            Threading::GetSystemTimes,
        },
    };
    // SAFETY: these C data structures admit a zero initial representation.
    let mut idle: FILETIME = unsafe { std::mem::zeroed() };
    let mut kernel = idle;
    let mut user = idle;
    let mut memory: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    memory.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    // SAFETY: all output pointers are valid for their documented structures.
    let cpu = (unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } != 0)
        .then(|| {
            ticks(kernel)
                .checked_add(ticks(user))
                .and_then(|total| Some((total.checked_sub(ticks(idle))?, total)))
        })
        .flatten();
    // SAFETY: writable initialized structure declares the exact supported length.
    let known = unsafe { GlobalMemoryStatusEx(&mut memory) } != 0;
    Host {
        cpu,
        memory_total: known.then_some(memory.ullTotalPhys),
        memory_used: known
            .then(|| memory.ullTotalPhys.checked_sub(memory.ullAvailPhys))
            .flatten(),
    }
}

#[cfg(unix)]
pub(super) fn process(pid: u32, expected_start: Option<u64>) -> Option<Process> {
    // Linux /proc only: other Unix systems return unknown. Not executed on Windows.
    let stat = bounded(&format!("/proc/{pid}/stat"), 8192)?;
    let fields: Vec<_> = stat[stat.rfind(')')? + 1..].split_whitespace().collect();
    if fields.len() < 22 || matches!(fields[0], "Z" | "X") {
        return None;
    }
    let start = fields[19].parse().ok()?;
    if expected_start.is_some_and(|expected| expected != start) {
        return None;
    }
    let cpu = fields[11]
        .parse::<u64>()
        .ok()?
        .checked_add(fields[12].parse().ok()?)?;
    let rss = fields[21].parse::<u64>().ok()?;
    let (hz, page) = unix_units()?;
    // Re-read identity after collecting fields, including memory, to reject a reused PID.
    if vesta_supervisor::native::process_start(pid) != Some(start) {
        return None;
    }
    Some(Process {
        start,
        cpu_seconds: cpu as f64 / hz as f64,
        memory: rss.checked_mul(page)?,
    })
}
#[cfg(unix)]
#[allow(unsafe_code)]
fn unix_units() -> Option<(u64, u64)> {
    // SAFETY: sysconf has no pointers; these names request clock/page constants.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    Some((
        u64::try_from(hz).ok().filter(|v| *v > 0)?,
        u64::try_from(page).ok().filter(|v| *v > 0)?,
    ))
}
#[cfg(unix)]
pub(super) fn bounded(path: &str, limit: u64) -> Option<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > limit {
        return None;
    }
    String::from_utf8(bytes).ok()
}
#[cfg(unix)]
pub(super) fn host() -> Host {
    let mut host = Host::default();
    if let Some(stat) = bounded("/proc/stat", 65536)
        && let Some(line) = stat.lines().find(|line| line.starts_with("cpu "))
    {
        let ticks: Option<Vec<u64>> = line
            .split_whitespace()
            .skip(1)
            .take(8)
            .map(|v| v.parse().ok())
            .collect();
        if let Some(ticks) = ticks.filter(|v| v.len() >= 4) {
            let total = ticks.iter().try_fold(0u64, |sum, v| sum.checked_add(*v));
            let idle = ticks[3].checked_add(ticks.get(4).copied().unwrap_or(0));
            host.cpu = total
                .zip(idle)
                .and_then(|(t, i)| Some((t.checked_sub(i)?, t)));
        }
    }
    if let Some(memory) = bounded("/proc/meminfo", 65536) {
        let get = |key: &str| {
            memory.lines().find_map(|line| {
                line.strip_prefix(key)?
                    .split_whitespace()
                    .next()?
                    .parse::<u64>()
                    .ok()?
                    .checked_mul(1024)
            })
        };
        host.memory_total = get("MemTotal:");
        host.memory_used = host
            .memory_total
            .zip(get("MemAvailable:"))
            .and_then(|(t, a)| t.checked_sub(a));
    }
    host
}

#[cfg(not(any(windows, unix)))]
pub(super) fn process(_: u32, _: Option<u64>) -> Option<Process> {
    None
}
#[cfg(not(any(windows, unix)))]
pub(super) fn host() -> Host {
    Host::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn process_cpu_uses_total_capacity() {
        assert_eq!(total_cpu_percent(5.0, 1.0, 10), Some(50.0));
        assert_eq!(total_cpu_percent(10.0, 1.0, 10), Some(100.0));
        assert_eq!(total_cpu_percent(12.0, 1.0, 10), Some(100.0));
        assert_eq!(total_cpu_percent(1.0, 1.0, 0), None);
    }
    #[test]
    fn metrics_native_identity_and_first_cpu_sample_are_honest() {
        let pid = std::process::id();
        let value = process(pid, None).expect("owned current process is observable");
        assert!(value.memory > 0);
        assert!(process(pid, Some(value.start.wrapping_add(1))).is_none());
        assert!(process(pid, Some(value.start)).is_some());
        let mut delta = CpuDelta::default();
        assert!(delta.observe(pid, Some(value), Instant::now()).is_none());
        assert!(delta.observe(pid, None, Instant::now()).is_none());
    }
}
