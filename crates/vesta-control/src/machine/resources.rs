//! Host resource planning for imports: a shared memory budget derived from
//! host RAM and game-port selection from 25565 upward. Planning is advisory:
//! reservations are recorded atomically at apply (H16/H17) and the actual bind
//! at start remains authoritative. Unknown inputs yield explicit `unresolved`
//! results with a blocker code, never a guess.
use crate::machine::imports::inspection::port_state;
use serde::Serialize;
use std::collections::BTreeSet;

pub const DEFAULT_GAME_PORT: u16 = 25_565;
/// Ports examined after the start port before giving up.
pub const PORT_SEARCH_LIMIT: u16 = 100;
pub const MIN_SERVER_MIB: u64 = 512;
pub const DEFAULT_MEMORY_MIB: u64 = 2_048;
pub const MODDED_MEMORY_MIB: u64 = 4_096;
const STEP_MIB: u64 = 256;
const MIB: u64 = 1024 * 1024;

/// Resources already committed to managed servers on this host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reservations {
    pub ports: BTreeSet<u16>,
    pub memory_mib: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MemoryPlan {
    pub total_mib: Option<u64>,
    pub os_reserve_mib: Option<u64>,
    pub budget_mib: Option<u64>,
    pub reserved_mib: u64,
    pub available_mib: Option<u64>,
    /// The requested or suggested heap; null when unresolved.
    pub memory_mib: Option<u64>,
    /// `resolved` or `unresolved`.
    pub status: &'static str,
    /// `memory_unresolved`, `memory_invalid` or `memory_budget_exceeded`.
    pub blocker: Option<&'static str>,
    pub warnings: Vec<&'static str>,
}

/// Operating system reserve: a quarter of RAM, between 1 GiB and 4 GiB.
pub fn os_reserve_mib(total_mib: u64) -> u64 {
    (total_mib / 4).clamp(1_024, 4_096)
}

/// Plan a 2 GiB default. Only the per-server budget is a hard limit;
/// reservations from other servers produce an advisory warning.
pub fn plan_memory(
    total_bytes: Option<u64>,
    reservations: &Reservations,
    requested_mib: Option<u64>,
) -> MemoryPlan {
    let unresolved = |blocker| MemoryPlan {
        total_mib: total_bytes.map(|b| b / MIB),
        os_reserve_mib: None,
        budget_mib: None,
        reserved_mib: reservations.memory_mib,
        available_mib: None,
        memory_mib: None,
        status: "unresolved",
        blocker: Some(blocker),
        warnings: Vec::new(),
    };
    let Some(total_mib) = total_bytes.map(|b| b / MIB).filter(|m| *m > 0) else {
        return unresolved("memory_unresolved");
    };
    let reserve = os_reserve_mib(total_mib);
    let budget = total_mib.saturating_sub(reserve);
    let available = budget.saturating_sub(reservations.memory_mib);
    let mut plan = MemoryPlan {
        total_mib: Some(total_mib),
        os_reserve_mib: Some(reserve),
        budget_mib: Some(budget),
        reserved_mib: reservations.memory_mib,
        available_mib: Some(available),
        memory_mib: None,
        status: "unresolved",
        blocker: None,
        warnings: Vec::new(),
    };
    let chosen = match requested_mib {
        Some(requested) if requested < MIN_SERVER_MIB => Err("memory_invalid"),
        Some(requested) if requested > budget => Err("memory_budget_exceeded"),
        Some(requested) => Ok(requested),
        None => {
            let suggested = DEFAULT_MEMORY_MIB.min(budget);
            let suggested = suggested / STEP_MIB * STEP_MIB;
            if suggested < MIN_SERVER_MIB {
                Err("memory_budget_exceeded")
            } else {
                Ok(suggested)
            }
        }
    };
    match chosen {
        Ok(mib) => {
            plan.memory_mib = Some(mib);
            plan.status = "resolved";
            if mib > available {
                plan.warnings.push("memory_overcommitted");
            }
        }
        Err(blocker) => plan.blocker = Some(blocker),
    }
    plan
}

/// Plan memory using this host's physical RAM.
pub fn plan_host_memory(reservations: &Reservations, requested_mib: Option<u64>) -> MemoryPlan {
    plan_memory(
        crate::metrics::host_memory_total_bytes(),
        reservations,
        requested_mib,
    )
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PortPlan {
    pub game_port: Option<u16>,
    /// `resolved` or `unresolved`.
    pub status: &'static str,
    /// `game_port_in_use`, `port_state_unknown` or `port_unresolved`.
    pub blocker: Option<&'static str>,
}

/// Choose a game port. A requested port must be unreserved and observed free;
/// otherwise the first unreserved, observed-free port from 25565 is suggested.
/// Ports whose state cannot be observed are skipped, never assumed free.
pub fn plan_port_with(
    reservations: &Reservations,
    requested: Option<u16>,
    probe: impl Fn(u16) -> &'static str,
) -> PortPlan {
    let resolved = |port| PortPlan {
        game_port: Some(port),
        status: "resolved",
        blocker: None,
    };
    let unresolved = |port, blocker| PortPlan {
        game_port: port,
        status: "unresolved",
        blocker: Some(blocker),
    };
    if let Some(port) = requested {
        if port == 0 || reservations.ports.contains(&port) {
            return unresolved(Some(port), "game_port_in_use");
        }
        return match probe(port) {
            "free" => resolved(port),
            "occupied" => unresolved(Some(port), "game_port_in_use"),
            _ => unresolved(Some(port), "port_state_unknown"),
        };
    }
    for offset in 0..=PORT_SEARCH_LIMIT {
        let Some(port) = DEFAULT_GAME_PORT.checked_add(offset) else {
            break;
        };
        if !reservations.ports.contains(&port) && probe(port) == "free" {
            return resolved(port);
        }
    }
    unresolved(None, "port_unresolved")
}

/// [`plan_port_with`] using live TCP observation on this host.
pub fn plan_port(reservations: &Reservations, requested: Option<u16>) -> PortPlan {
    let none = BTreeSet::new();
    plan_port_with(reservations, requested, |port| {
        port_state(port, "tcp", &none).state
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * MIB;

    #[test]
    fn memory_suggestion_uses_shared_budget() {
        let none = Reservations::default();
        let plan = plan_memory(Some(16 * GIB), &none, None);
        assert_eq!(plan.os_reserve_mib, Some(4_096));
        assert_eq!(plan.budget_mib, Some(12_288));
        assert_eq!(plan.memory_mib, Some(2_048));
        assert_eq!(plan.status, "resolved");
        // Small hosts keep the 1 GiB OS floor.
        let plan = plan_memory(Some(2 * GIB), &none, None);
        assert_eq!(plan.memory_mib, Some(1_024));
        // Existing reservations shrink what is offered.
        let busy = Reservations {
            ports: BTreeSet::new(),
            memory_mib: 11_000,
        };
        let plan = plan_memory(Some(16 * GIB), &busy, None);
        assert_eq!(plan.available_mib, Some(1_288));
        assert_eq!(plan.memory_mib, Some(2_048));
        let full = Reservations {
            ports: BTreeSet::new(),
            memory_mib: 12_000,
        };
        let plan = plan_memory(Some(16 * GIB), &full, None);
        assert_eq!(plan.blocker, None);
        assert_eq!(plan.memory_mib, Some(2_048));
        assert_eq!(plan.warnings, vec!["memory_overcommitted"]);
    }

    #[test]
    fn memory_requests_are_checked_and_unknown_ram_is_unresolved() {
        let none = Reservations::default();
        let plan = plan_memory(Some(8 * GIB), &none, Some(4_096));
        assert_eq!(plan.memory_mib, Some(4_096));
        let plan = plan_memory(Some(8 * GIB), &none, Some(7_000));
        assert_eq!(plan.blocker, Some("memory_budget_exceeded"));
        let plan = plan_memory(Some(8 * GIB), &none, Some(128));
        assert_eq!(plan.blocker, Some("memory_invalid"));
        let plan = plan_memory(None, &none, Some(2_048));
        assert_eq!(plan.status, "unresolved");
        assert_eq!(plan.blocker, Some("memory_unresolved"));
        assert!(plan_host_memory(&none, None).total_mib.is_some());
    }

    #[test]
    fn ports_start_at_25565_and_skip_reserved_busy_and_unknown() {
        let reserved = Reservations {
            ports: BTreeSet::from([25_565]),
            memory_mib: 0,
        };
        let probe = |port: u16| match port {
            25_566 => "occupied",
            25_567 => "unknown",
            _ => "free",
        };
        assert_eq!(
            plan_port_with(&reserved, None, probe).game_port,
            Some(25_568)
        );
        assert_eq!(
            plan_port_with(&Reservations::default(), None, |_| "free").game_port,
            Some(25_565)
        );
        let requested = plan_port_with(&reserved, Some(25_565), probe);
        assert_eq!(requested.blocker, Some("game_port_in_use"));
        assert_eq!(
            plan_port_with(&reserved, Some(25_567), probe).blocker,
            Some("port_state_unknown")
        );
        assert_eq!(
            plan_port_with(&reserved, Some(25_570), probe).game_port,
            Some(25_570)
        );
        let exhausted = plan_port_with(&Reservations::default(), None, |_| "occupied");
        assert_eq!(exhausted.status, "unresolved");
        assert_eq!(exhausted.blocker, Some("port_unresolved"));
    }

    #[test]
    fn live_port_plan_avoids_a_listening_port() {
        let listener = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let plan = plan_port(&Reservations::default(), Some(port));
        assert_eq!(plan.blocker, Some("game_port_in_use"));
    }
}
