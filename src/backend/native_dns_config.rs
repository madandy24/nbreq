//! Supported-platform discovery of recursive DNS servers.
//!
//! This module reads configuration only. The NBReq-owned resolver remains responsible for every
//! query socket, retry, deadline, cancellation, and shutdown join.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr, SocketAddrV6};
use std::time::Duration;

use crate::{Error, ErrorKind};

const DNS_PORT: u16 = 53;
#[cfg(windows)]
const DEFAULT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(1);
const DEFAULT_ATTEMPTS: u8 = 3;

#[derive(Debug)]
pub(super) struct DiscoveredResolverConfig {
    pub(super) nameservers: Vec<SocketAddr>,
    pub(super) attempt_timeout: Duration,
    pub(super) attempts: u8,
}

pub(super) fn discover() -> Result<DiscoveredResolverConfig, Error> {
    platform::discover()
}

fn finish(
    nameservers: impl IntoIterator<Item = SocketAddr>,
    attempt_timeout: Duration,
    attempts: u8,
) -> Result<DiscoveredResolverConfig, Error> {
    let mut seen = HashSet::new();
    let nameservers = nameservers
        .into_iter()
        .filter(|address| !address.ip().is_unspecified())
        .filter(|address| seen.insert(*address))
        .collect::<Vec<_>>();
    if nameservers.is_empty() {
        return Err(Error::new(
            ErrorKind::Internal,
            "the operating system reported no usable DNS nameservers",
        ));
    }
    Ok(DiscoveredResolverConfig {
        nameservers,
        attempt_timeout,
        attempts,
    })
}

#[cfg(windows)]
mod platform {
    use ipconfig::OperStatus;

    use super::*;

    struct RankedServer {
        metric: u32,
        adapter_order: usize,
        server_order: usize,
        address: SocketAddr,
    }

    pub(super) fn discover() -> Result<DiscoveredResolverConfig, Error> {
        let adapters = ipconfig::get_adapters().map_err(|error| {
            Error::new(
                ErrorKind::Internal,
                format!("Windows DNS adapter discovery failed: {error}"),
            )
        })?;
        let mut ranked = Vec::new();
        for (adapter_order, adapter) in adapters.into_iter().enumerate() {
            if adapter.oper_status() != OperStatus::IfOperStatusUp {
                continue;
            }
            for (server_order, address) in adapter.dns_servers().iter().copied().enumerate() {
                let (metric, address) = match address {
                    IpAddr::V4(address) => (
                        adapter.ipv4_metric(),
                        SocketAddr::new(IpAddr::V4(address), DNS_PORT),
                    ),
                    IpAddr::V6(address) => {
                        let scope_id = if address.is_unicast_link_local() {
                            adapter.ipv6_if_index()
                        } else {
                            0
                        };
                        (
                            adapter.ipv6_metric(),
                            SocketAddr::V6(SocketAddrV6::new(address, DNS_PORT, 0, scope_id)),
                        )
                    }
                };
                ranked.push(RankedServer {
                    metric,
                    adapter_order,
                    server_order,
                    address,
                });
            }
        }
        ranked.sort_by_key(|server| (server.metric, server.adapter_order, server.server_order));
        finish(
            ranked.into_iter().map(|server| server.address),
            DEFAULT_ATTEMPT_TIMEOUT,
            DEFAULT_ATTEMPTS,
        )
    }
}

#[cfg(unix)]
mod platform {
    use std::fs;

    use resolv_conf::ScopedIp;

    use super::*;

    pub(super) fn discover() -> Result<DiscoveredResolverConfig, Error> {
        let bytes = fs::read("/etc/resolv.conf").map_err(|error| {
            Error::new(
                ErrorKind::Internal,
                format!("system DNS configuration could not be read: {error}"),
            )
        })?;
        let config = resolv_conf::Config::parse(&bytes).map_err(|error| {
            Error::new(
                ErrorKind::Internal,
                format!("system DNS configuration could not be parsed: {error}"),
            )
        })?;
        let nameservers = config
            .nameservers
            .into_iter()
            .filter_map(|address| match address {
                ScopedIp::V4(address) => Some(SocketAddr::new(IpAddr::V4(address), DNS_PORT)),
                ScopedIp::V6(address, None) => {
                    Some(SocketAddr::V6(SocketAddrV6::new(address, DNS_PORT, 0, 0)))
                }
                // A textual interface scope needs an if_nametoindex-style platform call. Do not
                // silently discard it and send a link-local query on the wrong interface.
                ScopedIp::V6(_, Some(_)) => None,
            });
        let timeout = Duration::from_secs(u64::from(config.timeout.clamp(1, 5)));
        let attempts = u8::try_from(config.attempts.clamp(1, 5)).unwrap_or(DEFAULT_ATTEMPTS);
        finish(nameservers, timeout, attempts)
    }
}

#[cfg(not(any(windows, unix)))]
mod platform {
    use super::*;

    pub(super) fn discover() -> Result<DiscoveredResolverConfig, Error> {
        Err(Error::new(
            ErrorKind::Unsupported,
            "system DNS discovery is supported only on Windows and Unix targets",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_deduplicates_and_rejects_unspecified_servers() {
        let first: SocketAddr = "192.0.2.53:53".parse().expect("fixture address");
        let result = finish(
            [
                "0.0.0.0:53".parse().expect("unspecified fixture"),
                first,
                first,
                "[2001:db8::53]:53".parse().expect("IPv6 fixture"),
            ],
            Duration::from_secs(2),
            4,
        )
        .expect("usable system configuration");
        assert_eq!(result.nameservers.len(), 2);
        assert_eq!(result.nameservers[0], first);
        assert_eq!(result.attempt_timeout, Duration::from_secs(2));
        assert_eq!(result.attempts, 4);
    }

    #[test]
    fn current_platform_reports_at_least_one_system_nameserver() {
        let discovered = discover().expect("this supported test host must have system DNS");
        assert!(!discovered.nameservers.is_empty());
    }

    #[test]
    fn system_dns_and_platform_tls_factory_owns_and_joins_its_resolver() {
        let engine =
            crate::testing::native_https_engine_with_system_dns(crate::EngineConfig::spawned())
                .expect("system DNS/native TLS proving Engine must construct");
        engine
            .shutdown()
            .expect("system DNS/native TLS proving Engine must join");
    }
}
