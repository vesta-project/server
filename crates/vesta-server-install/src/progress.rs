//! Host-owned progress and live control, not client-supplied installation policy.

pub trait ProgressReporter: Send + Sync {
    fn start_step(&self, name: &str, total_steps: Option<u32>);
    fn update_bytes(&self, transferred: u64, total: Option<u64>);
    fn done(&self, success: bool, message: Option<&str>);
    fn is_cancelled(&self) -> bool;
    fn is_paused(&self) -> bool;
    fn is_dry_run(&self) -> bool {
        false
    }
}

#[derive(Default)]
pub struct SilentProgressReporter;
impl ProgressReporter for SilentProgressReporter {
    fn start_step(&self, _: &str, _: Option<u32>) {}
    fn update_bytes(&self, _: u64, _: Option<u64>) {}
    fn done(&self, _: bool, _: Option<&str>) {}
    fn is_cancelled(&self) -> bool {
        false
    }
    fn is_paused(&self) -> bool {
        false
    }
}
