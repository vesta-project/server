//! Passive host interface counters. A membership or counter discontinuity invalidates
//! the entire interval; totals cannot hide resets behind another interface's traffic.
use std::{collections::BTreeMap, time::Instant};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Counters {
    pub received: u64,
    pub sent: u64,
}
pub(super) type Interfaces = BTreeMap<String, Counters>;
#[derive(Default)]
pub(super) struct Delta {
    previous: Option<(Interfaces, Instant)>,
}
impl Delta {
    pub fn observe(
        &mut self,
        current: Option<Interfaces>,
        now: Instant,
    ) -> (Option<f64>, Option<f64>) {
        let Some(current) = current else {
            self.previous = None;
            return (None, None);
        };
        let Some((old, before)) = self.previous.replace((current.clone(), now)) else {
            return (None, None);
        };
        let compute = || {
            if old.is_empty() || !old.keys().eq(current.keys()) {
                return None;
            }
            let seconds = now.checked_duration_since(before)?.as_secs_f64();
            if seconds <= 0.0 {
                return None;
            }
            let (rx, tx) = current
                .iter()
                .try_fold((0u64, 0u64), |(rx, tx), (key, value)| {
                    let before = old.get(key)?;
                    Some((
                        rx.checked_add(value.received.checked_sub(before.received)?)?,
                        tx.checked_add(value.sent.checked_sub(before.sent)?)?,
                    ))
                })?;
            Some((rx as f64 / seconds, tx as f64 / seconds))
        };
        compute().map_or((None, None), |(rx, tx)| (Some(rx), Some(tx)))
    }
}

// Apply the same whitelist on both platforms. It excludes loopback (24/772),
// tunnels (131/768/769/776/778), virtual point-to-point links, and unknown types.
// Physical Ethernet and Wi-Fi counters include host traffic, not a Minecraft-port
// estimate. Virtual bridges/VPN Ethernet are excluded to avoid double counting.
fn eligible(up: bool, ethernet_or_wifi: bool) -> bool {
    up && ethernet_or_wifi
}

#[cfg(windows)]
fn physical_windows_flags(flags: u8) -> bool {
    // windows-sys 0.61.2 exposes only _bitfield, no accessors. netioapi.h declares
    // HardwareInterface, FilterInterface, ConnectorPresent as bits 0, 1, 2.
    // https://learn.microsoft.com/en-us/windows/win32/api/netioapi/ns-netioapi-mib_if_row2
    flags & 0b111 == 0b101
}
#[cfg(windows)]
#[allow(unsafe_code)]
pub(super) fn interfaces() -> Option<Interfaces> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        FreeMibTable, GetIfTable2, MIB_IF_TABLE2,
    };
    let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
    // SAFETY: GetIfTable2 initializes an OS-owned allocation through a valid pointer.
    if unsafe { GetIfTable2(&mut table) } != 0 || table.is_null() {
        return None;
    }
    let result = (|| {
        // SAFETY: success supplies a valid table whose trailing array contains NumEntries rows.
        let count = unsafe { (*table).NumEntries } as usize;
        if count > 4096 {
            return None;
        }
        // SAFETY: Table is the first element of the documented trailing array.
        let rows =
            unsafe { std::slice::from_raw_parts(std::ptr::addr_of!((*table).Table).cast(), count) };
        let mut interfaces = Interfaces::new();
        let mut guids = std::collections::BTreeSet::new();
        for row in rows {
            let row: &windows_sys::Win32::NetworkManagement::IpHelper::MIB_IF_ROW2 = row;
            if eligible(
                row.OperStatus == 1,
                matches!(row.Type, 6 | 71)
                    && row.TunnelType == 0
                    && physical_windows_flags(row.InterfaceAndOperStatusFlags._bitfield),
            ) {
                // LUID is stable across interface-index reuse; bytes belong to this incarnation.
                // SAFETY: Value is the scalar representation of the NET_LUID_LH union.
                let luid = unsafe { row.InterfaceLuid.Value };
                let g = row.InterfaceGuid;
                let guid = format!(
                    "{:08x}-{:04x}-{:04x}-{:02x?}",
                    g.data1, g.data2, g.data3, g.data4
                );
                // A duplicate adapter identity invalidates rather than doubles a total.
                if !guids.insert(guid.clone()) {
                    return None;
                }
                interfaces.insert(
                    format!("{guid}:{luid}:{}", row.Type),
                    Counters {
                        received: row.InOctets,
                        sent: row.OutOctets,
                    },
                );
            }
        }
        Some(interfaces)
    })();
    // SAFETY: allocation was obtained from GetIfTable2 and is freed exactly once.
    unsafe {
        FreeMibTable(table.cast());
    }
    result
}

#[cfg(target_os = "linux")]
pub(super) fn interfaces() -> Option<Interfaces> {
    let net = super::native::bounded("/proc/net/dev", 65536)?;
    parse_linux(&net, |name| {
        if name.is_empty() || name.len() > 64 || name.contains('/') || name == "." || name == ".." {
            return None;
        }
        let prefix = format!("/sys/class/net/{name}");
        let kind: u32 = super::native::bounded(&format!("{prefix}/type"), 64)?
            .trim()
            .parse()
            .ok()?;
        let flags = super::native::bounded(&format!("{prefix}/flags"), 64)?;
        let flags = u32::from_str_radix(flags.trim().trim_start_matches("0x"), 16).ok()?;
        let index = super::native::bounded(&format!("{prefix}/ifindex"), 64)?;
        let up = super::native::bounded(&format!("{prefix}/operstate"), 64)?;
        Some((
            format!("{}:{name}:{kind}", index.trim()),
            eligible(
                up.trim() == "up" && flags & 1 != 0 && flags & (8 | 16) == 0,
                kind == 1 && std::fs::metadata(format!("{prefix}/device")).is_ok(),
            ),
        ))
    })
}
#[cfg(any(target_os = "linux", test))]
fn parse_linux(
    net: &str,
    mut identity: impl FnMut(&str) -> Option<(String, bool)>,
) -> Option<Interfaces> {
    let mut interfaces = Interfaces::new();
    for line in net.lines().skip(2) {
        let (name, values) = line.split_once(':')?;
        let (id, include) = identity(name.trim())?;
        if !include {
            continue;
        }
        let fields: Vec<_> = values.split_whitespace().collect();
        if fields.len() != 16 {
            return None;
        }
        let value = Counters {
            received: fields[0].parse().ok()?,
            sent: fields[8].parse().ok()?,
        };
        if interfaces.insert(id, value).is_some() {
            return None;
        }
    }
    Some(interfaces)
}
#[cfg(not(any(windows, target_os = "linux")))]
pub(super) fn interfaces() -> Option<Interfaces> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    fn counters(rx: u64, tx: u64) -> Interfaces {
        BTreeMap::from([(
            "eth:1".into(),
            Counters {
                received: rx,
                sent: tx,
            },
        )])
    }
    #[cfg(windows)]
    #[test]
    fn network_windows_native_snapshot_is_repeatable_and_has_unique_adapter_guids() {
        for _ in 0..2 {
            let snapshot = interfaces().expect("native interface table read succeeds");
            let mut guids = std::collections::BTreeSet::new();
            for key in snapshot.keys() {
                assert!(guids.insert(key.split(':').next().unwrap()));
            }
            // An offline host may have no eligible adapters.
        }
        assert!(physical_windows_flags(0b101));
        assert!(physical_windows_flags(0b1100_0101));
        assert!(!physical_windows_flags(0b111)); // NDIS filter/LWF layer.
        assert!(!physical_windows_flags(0b001)); // No physical connector.
    }
    #[test]
    fn network_rates_reset_wrap_churn_and_failed_read_are_unknown() {
        let mut delta = Delta::default();
        let at = Instant::now();
        assert_eq!(delta.observe(Some(counters(10, 20)), at), (None, None));
        assert_eq!(
            delta.observe(Some(counters(30, 50)), at + Duration::from_secs(2)),
            (Some(10.0), Some(15.0))
        );
        assert_eq!(
            delta.observe(Some(counters(1, 60)), at + Duration::from_secs(4)),
            (None, None)
        );
        assert_eq!(
            delta
                .observe(Some(counters(u64::MAX, 70)), at + Duration::from_secs(6))
                .1,
            Some(5.0)
        );
        assert_eq!(
            delta.observe(Some(counters(0, 80)), at + Duration::from_secs(8)),
            (None, None)
        );
        let mut changed = counters(10, 100);
        changed.insert(
            "wifi:2".into(),
            Counters {
                received: 1,
                sent: 1,
            },
        );
        assert_eq!(
            delta.observe(Some(changed), at + Duration::from_secs(10)),
            (None, None)
        );
        assert_eq!(
            delta.observe(None, at + Duration::from_secs(12)),
            (None, None)
        );
        assert_eq!(
            delta.observe(Some(counters(20, 200)), at + Duration::from_secs(14)),
            (None, None)
        );
    }
    #[test]
    fn network_linux_parser_excludes_down_loopback_and_tunnel_and_rejects_bad_counters() {
        let fixture = "header\nheader\n eth0: 10 0 0 0 0 0 0 0 20 0 0 0 0 0 0 0\n lo: 999 0 0 0 0 0 0 0 999 0 0 0 0 0 0 0\n tun0: 999 0 0 0 0 0 0 0 999 0 0 0 0 0 0 0\n down: 999 0 0 0 0 0 0 0 999 0 0 0 0 0 0 0";
        let interfaces = parse_linux(fixture, |name| Some((name.into(), name == "eth0"))).unwrap();
        assert_eq!(interfaces.len(), 1);
        assert_eq!(interfaces["eth0"].received, 10);
        assert!(parse_linux("h\nh\neth0: invalid", |name| Some((name.into(), true))).is_none());
        assert!(!eligible(false, true));
        assert!(!eligible(true, false));
        assert!(eligible(true, true));
    }
}
