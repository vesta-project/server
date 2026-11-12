use super::{Error, Result};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub sessions: usize,
    pub sessions_per_ip: usize,
    pub handles_per_session: usize,
    pub transfers_per_principal: usize,
    pub bytes_per_second: u64,
    pub bytes_per_session: u64,
    pub bytes_per_principal_day: u64,
    pub max_file_bytes: u64,
    pub idle_timeout_secs: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            sessions: 8,
            sessions_per_ip: 2,
            handles_per_session: 16,
            transfers_per_principal: 4,
            bytes_per_second: 8 * 1024 * 1024,
            bytes_per_session: 1024 * 1024 * 1024,
            bytes_per_principal_day: 4 * 1024 * 1024 * 1024,
            max_file_bytes: 16 * 1024 * 1024,
            idle_timeout_secs: 120,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub enabled: bool,
    pub bind_address: IpAddr,
    pub port: u16,
    pub allow_cidrs: Vec<IpNet>,
    pub deny_cidrs: Vec<IpNet>,
    pub limits: Limits,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_address: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 2222,
            allow_cidrs: vec![],
            deny_cidrs: vec![],
            limits: Limits::default(),
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        let l = &self.limits;
        if self.port == 0
            || self.allow_cidrs.len() > 64
            || self.deny_cidrs.len() > 64
            || !(1..=64).contains(&l.sessions)
            || !(1..=l.sessions).contains(&l.sessions_per_ip)
            || !(1..=64).contains(&l.handles_per_session)
            || !(1..=16).contains(&l.transfers_per_principal)
            || !(65536..=64 * 1024 * 1024).contains(&l.bytes_per_second)
            || !(65536..=16 * 1024 * 1024 * 1024).contains(&l.bytes_per_session)
            || !(65536..=64 * 1024 * 1024 * 1024).contains(&l.bytes_per_principal_day)
            || !(1..=crate::files::MAX_WRITE_BYTES as u64).contains(&l.max_file_bytes)
            || !(10..=600).contains(&l.idle_timeout_secs)
        {
            return Err(Error("sftp_configuration_invalid"));
        }
        Ok(())
    }
    pub(super) fn permits(&self, ip: IpAddr) -> bool {
        !self.deny_cidrs.iter().any(|n| n.contains(&ip))
            && (self.allow_cidrs.is_empty() || self.allow_cidrs.iter().any(|n| n.contains(&ip)))
    }
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConfigPatch {
    pub enabled: Option<bool>,
    pub bind_address: Option<IpAddr>,
    pub port: Option<u16>,
    pub allow_cidrs: Option<Vec<IpNet>>,
    pub deny_cidrs: Option<Vec<IpNet>>,
    pub limits: Option<LimitsPatch>,
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimitsPatch {
    pub sessions: Option<usize>,
    pub sessions_per_ip: Option<usize>,
    pub handles_per_session: Option<usize>,
    pub transfers_per_principal: Option<usize>,
    pub bytes_per_second: Option<u64>,
    pub bytes_per_session: Option<u64>,
    pub bytes_per_principal_day: Option<u64>,
    pub max_file_bytes: Option<u64>,
    pub idle_timeout_secs: Option<u64>,
}
macro_rules! apply {($from:expr,$to:expr,$($field:ident),*)=>{$(if let Some(v)=$from.$field { $to.$field=v; })*};}
impl ConfigPatch {
    pub(super) fn apply(self, c: &mut Config) {
        apply!(
            self,
            c,
            enabled,
            bind_address,
            port,
            allow_cidrs,
            deny_cidrs
        );
        if let Some(l) = self.limits {
            apply!(
                l,
                c.limits,
                sessions,
                sessions_per_ip,
                handles_per_session,
                transfers_per_principal,
                bytes_per_second,
                bytes_per_session,
                bytes_per_principal_day,
                max_file_bytes,
                idle_timeout_secs
            );
        }
    }
}
