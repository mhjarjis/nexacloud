//! Host metrics via sysinfo.

use std::sync::Mutex;
use std::time::Duration;

use axum::extract::State;
use axum::Json;
use serde::Serialize;
use sysinfo::{Disks, System};

use crate::state::AppState;

#[derive(Debug, Clone, Serialize)]
pub struct MemInfo {
    pub total: u64,
    pub used: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DiskInfo {
    pub mount: String,
    pub total: u64,
    pub used: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SystemInfo {
    pub hostname: String,
    pub os: String,
    pub kernel: String,
    pub uptime_secs: u64,
    pub cpu_count: usize,
    pub cpu_percent: f32,
    pub load: [f64; 3],
    pub memory: MemInfo,
    pub swap: MemInfo,
    pub disks: Vec<DiskInfo>,
}

/// Background CPU sampler: cpu usage needs two measurements some time apart,
/// so a task refreshes it every few seconds and handlers read the last value.
#[derive(Default)]
pub struct CpuSampler {
    value: Mutex<(f32, usize)>,
}

impl CpuSampler {
    pub fn get(&self) -> (f32, usize) {
        *self.value.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn spawn(state: AppState) {
        tokio::spawn(async move {
            let Ok(mut sys) = System::new() else {
                tracing::warn!("cpu sampling unavailable");
                return;
            };
            sys.refresh_cpu_all();
            tokio::time::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL.max(Duration::from_millis(500))).await;
            loop {
                sys.refresh_cpu_usage();
                let v = sys.global_cpu_usage();
                let n = sys.cpus().len();
                *state.cpu.value.lock().unwrap_or_else(|e| e.into_inner()) =
                    (if v.is_finite() { (v * 10.0).round() / 10.0 } else { 0.0 }, n);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }
}

const SKIP_FS: [&str; 14] = [
    "tmpfs", "devtmpfs", "squashfs", "overlay", "efivarfs", "ramfs", "autofs", "proc", "sysfs",
    "nsfs", "iso9660", "devfs", "cgroup2", "fuse.snapfuse",
];

/// (mount, fs, total, available) → disks we report.
pub fn filter_disks(raw: Vec<(String, String, u64, u64)>) -> Vec<DiskInfo> {
    let mut out: Vec<DiskInfo> = Vec::new();
    for (mount, fs, total, avail) in raw {
        if total == 0
            || SKIP_FS.contains(&fs.as_str())
            // /etc/*: single-file bind mounts (containers: resolv.conf, hosts, hostname)
            || ["/snap/", "/proc", "/sys", "/dev", "/run", "/etc/"]
                .iter()
                .any(|p| mount.starts_with(p))
            || out.iter().any(|d| d.mount == mount)
        {
            continue;
        }
        out.push(DiskInfo {
            mount,
            total,
            used: total.saturating_sub(avail),
        });
    }
    // The root filesystem first, then the rest by mount path.
    out.sort_by(|a, b| (a.mount != "/", &a.mount).cmp(&(b.mount != "/", &b.mount)));
    out
}

#[cfg(test)]
mod disk_tests {
    use super::filter_disks;

    #[test]
    fn root_first_and_file_bind_mounts_skipped() {
        let g = 1u64 << 30;
        let d = filter_disks(vec![
            ("/etc/resolv.conf".into(), "ext4".into(), 100 * g, 50 * g),
            ("/home".into(), "ext4".into(), 10 * g, 5 * g),
            ("/".into(), "ext4".into(), 100 * g, 60 * g),
            ("/boot/efi".into(), "vfat".into(), g, g / 2),
        ]);
        let mounts: Vec<&str> = d.iter().map(|x| x.mount.as_str()).collect();
        assert_eq!(mounts, ["/", "/boot/efi", "/home"]);
    }
}

pub fn disks() -> Vec<DiskInfo> {
    let Ok(disks) = Disks::new_with_refreshed_list() else {
        return Vec::new();
    };
    let raw = disks
        .iter()
        .map(|d| {
            (
                d.mount_point().to_string_lossy().into_owned(),
                d.file_system().to_string_lossy().into_owned(),
                d.total_space(),
                d.available_space(),
            )
        })
        .collect();
    filter_disks(raw)
}

pub fn hostname() -> String {
    System::host_name().unwrap_or_else(|_| "unknown".into())
}

fn os_name() -> String {
    if let Ok(s) = std::fs::read_to_string("/etc/os-release") {
        for line in s.lines() {
            if let Some(v) = line.strip_prefix("PRETTY_NAME=") {
                return v.trim().trim_matches('"').to_string();
            }
        }
    }
    System::long_os_version().unwrap_or_else(|_| "unknown".into())
}

pub fn collect(cpu: (f32, usize)) -> SystemInfo {
    let (memory, swap, cpus) = match System::new() {
        Ok(mut sys) => {
            sys.refresh_memory();
            (
                MemInfo { total: sys.total_memory(), used: sys.used_memory() },
                MemInfo { total: sys.total_swap(), used: sys.used_swap() },
                cpu.1,
            )
        }
        Err(_) => (MemInfo { total: 0, used: 0 }, MemInfo { total: 0, used: 0 }, cpu.1),
    };
    let load = System::load_average()
        .map(|l| [l.one, l.five, l.fifteen])
        .unwrap_or([0.0; 3]);
    let cpu_count = if cpus > 0 {
        cpus
    } else {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0)
    };
    SystemInfo {
        hostname: hostname(),
        os: os_name(),
        kernel: System::kernel_version().unwrap_or_else(|_| "unknown".into()),
        uptime_secs: System::uptime().unwrap_or(0),
        cpu_count,
        cpu_percent: cpu.0,
        load,
        memory,
        swap,
        disks: disks(),
    }
}

pub async fn get(State(state): State<AppState>) -> Json<SystemInfo> {
    let cpu = state.cpu.get();
    Json(
        tokio::task::spawn_blocking(move || collect(cpu))
            .await
            .unwrap_or_else(|_| collect_fallback()),
    )
}

fn collect_fallback() -> SystemInfo {
    SystemInfo {
        hostname: hostname(),
        os: "unknown".into(),
        kernel: "unknown".into(),
        uptime_secs: 0,
        cpu_count: 0,
        cpu_percent: 0.0,
        load: [0.0; 3],
        memory: MemInfo { total: 0, used: 0 },
        swap: MemInfo { total: 0, used: 0 },
        disks: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_filtering() {
        let raw = vec![
            ("/".into(), "ext4".into(), 100, 40),
            ("/snap/core22/1".into(), "squashfs".into(), 10, 0),
            ("/run".into(), "tmpfs".into(), 10, 5),
            ("/boot/efi".into(), "vfat".into(), 10, 9),
            ("/".into(), "ext4".into(), 100, 40),
            ("/empty".into(), "ext4".into(), 0, 0),
        ];
        assert_eq!(
            filter_disks(raw),
            vec![
                DiskInfo { mount: "/".into(), total: 100, used: 60 },
                DiskInfo { mount: "/boot/efi".into(), total: 10, used: 1 },
            ]
        );
    }
}
