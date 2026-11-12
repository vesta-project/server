//! Explicitly opt-in diagnostic counters, scoped to one blocking request thread.
use std::{cell::RefCell, collections::BTreeMap, time::Instant};
#[derive(Default, serde::Serialize)]
pub struct Metric {
    pub calls: u64,
    pub nanos: u64,
    pub bytes: u64,
}
#[derive(Default)]
struct Probe {
    active: bool,
    metrics: BTreeMap<&'static str, Metric>,
}
thread_local! { static PROBE: RefCell<Probe> = RefCell::new(Probe::default()); }
pub fn begin() {
    PROBE.with(|p| {
        *p.borrow_mut() = Probe {
            active: true,
            ..Probe::default()
        }
    });
}
pub fn finish() -> BTreeMap<&'static str, Metric> {
    PROBE.with(|p| {
        let mut p = p.borrow_mut();
        p.active = false;
        std::mem::take(&mut p.metrics)
    })
}
pub fn count(name: &'static str, bytes: u64) {
    PROBE.with(|p| {
        let mut p = p.borrow_mut();
        if p.active {
            let m = p.metrics.entry(name).or_default();
            m.calls += 1;
            m.bytes += bytes;
        }
    });
}
pub struct Span {
    name: &'static str,
    start: Option<Instant>,
}
impl Span {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            start: PROBE.with(|p| p.borrow().active.then(Instant::now)),
        }
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        if let Some(start) = self.start {
            PROBE.with(|p| {
                let mut p = p.borrow_mut();
                if p.active {
                    let m = p.metrics.entry(self.name).or_default();
                    m.calls += 1;
                    m.nanos += start.elapsed().as_nanos().min(u64::MAX as u128) as u64;
                }
            });
        }
    }
}
