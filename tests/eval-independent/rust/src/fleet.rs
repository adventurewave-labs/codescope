//! Fleet: generic fan-out over probes.

use crate::probe::{Probe, TcpProbe};

pub struct Fleet {
    pub backlog: Vec<String>,
}

impl Fleet {
    pub fn sweep(&mut self, hosts: Vec<String>) -> u64 {
        let probe = TcpProbe::new(250); // @eval new=TcpProbe::new@src/probe.rs
        let mut worst = 0;
        for h in hosts {
            let latency = probe.ping(&h); // @eval ping=TcpProbe::ping@src/probe.rs
            worst = worst.max(latency);
        }
        self.drain(); // @eval drain=Fleet::drain@src/fleet.rs
        worst
    }

    fn drain(&mut self) {
        self.backlog.clear(); // @eval clear=-
    }
}

fn map_all<T: Copy, F: Fn(T) -> T>(items: Vec<T>, f: F) -> Vec<T> {
    let mut out = Vec::with_capacity(items.len()); // @eval with_capacity=-
    for it in items {
        out.push(f(it));
    }
    out
}
