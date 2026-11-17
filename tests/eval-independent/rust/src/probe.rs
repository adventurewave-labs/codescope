//! Probe trait: exactly one in-repo impl (TcpProbe) ships with the crate.

pub trait Probe {
    fn ping(&self, host: &str) -> u64;
    fn name(&self) -> &'static str;
}

pub struct TcpProbe {
    pub timeout_ms: u64,
}

impl TcpProbe {
    pub fn new(timeout_ms: u64) -> Self {
        Self { timeout_ms }
    }
}

impl Probe for TcpProbe {
    fn ping(&self, host: &str) -> u64 {
        let start = measure(); // @eval measure=measure@src/probe.rs
        if host.is_empty() { // @eval is_empty=-
            return 0;
        }
        self.timeout_ms.saturating_sub(start)
    }

    fn name(&self) -> &'static str {
        "tcp"
    }
}

fn measure() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64) // @eval as_millis=-
        .unwrap_or(0)
}
