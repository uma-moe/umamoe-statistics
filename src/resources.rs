use anyhow::{anyhow, Result};
use std::time::Duration;
use sysinfo::{get_current_pid, Pid, System};

include!("types/resources.rs");

impl ResourceMonitor {
    pub(crate) fn new() -> Result<Self> {
        let pid = get_current_pid().map_err(|error| anyhow!(error))?;
        let mut system = System::new_all();
        system.refresh_all();

        Ok(Self {
            system,
            pid,
            peak_exporter_memory: 0,
        })
    }

    fn summary(&mut self) -> String {
        self.system.refresh_all();
        let cpu_scale = self.system.cpus().len().max(1) as f32;

        let process = self.system.process(self.pid);
        let exporter_cpu = process.map_or(0.0, |process| process.cpu_usage()) / cpu_scale;
        let exporter_memory = process.map_or(0, |process| process.memory());
        self.peak_exporter_memory = self.peak_exporter_memory.max(exporter_memory);

        let mut postgres_count = 0_u64;
        let mut postgres_cpu = 0.0_f32;
        let mut postgres_memory = 0_u64;
        for process in self.system.processes().values() {
            if process.name().to_ascii_lowercase().contains("postgres") {
                postgres_count += 1;
                postgres_cpu += process.cpu_usage();
                postgres_memory += process.memory();
            }
        }
        postgres_cpu /= cpu_scale;

        format!(
            "cpu exporter {:.1}% postgres {:.1}% system {:.1}% | mem exporter {:.1} MB peak {:.1} MB postgres {:.1} MB/{} proc system {:.1}/{:.1} GB",
            exporter_cpu,
            postgres_cpu,
            self.system.global_cpu_info().cpu_usage(),
            bytesToMb(exporter_memory),
            bytesToMb(self.peak_exporter_memory),
            bytesToMb(postgres_memory),
            postgres_count,
            bytesToGb(self.system.used_memory()),
            bytesToGb(self.system.total_memory())
        )
    }
}

pub(crate) fn printProgress(
    resource_monitor: &mut Option<ResourceMonitor>,
    label: &str,
    rows: u64,
    elapsed: Duration,
) {
    let elapsed_secs = elapsed.as_secs_f64();
    let rows_per_second = if elapsed_secs > 0.0 {
        rows as f64 / elapsed_secs
    } else {
        0.0
    };

    if let Some(resource_monitor) = resource_monitor {
        println!(
            "  {label} {rows:>12} rows in {elapsed_secs:.1}s ({rows_per_second:.0} rows/s) | {}",
            resource_monitor.summary()
        );
    } else {
        println!("  {label} {rows:>12} rows in {elapsed_secs:.1}s ({rows_per_second:.0} rows/s)");
    }
}

fn bytesToMb(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

fn bytesToGb(bytes: u64) -> f64 {
    bytes as f64 / 1_073_741_824.0
}
