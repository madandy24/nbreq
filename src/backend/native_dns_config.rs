//! Supported-platform discovery of recursive DNS servers and optional public-resolver search
//! suffixes.
//!
//! This module reads configuration only. The NBReq-owned resolver remains responsible for every
//! query socket, retry, deadline, cancellation, shutdown join, and FQ-10 candidate algorithm.
//! Ordinary system discovery uses platform-owned configuration on Windows, Linux, and the bounded
//! default-resolver shape supported on macOS. The default-on `resolver` feature adds public
//! search-suffix discovery; native-only builds retain nameserver discovery for exact HTTP and TCP
//! lookups. Other Unix targets fail closed rather than inheriting Linux `/etc/resolv.conf`
//! semantics. Injected nameserver and suffix fixtures remain usable everywhere.

use std::collections::HashSet;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
use std::net::IpAddr;
use std::net::SocketAddr;
#[cfg(any(windows, target_os = "linux"))]
use std::net::SocketAddrV6;
use std::time::Duration;

use crate::dns::normalize_dns_name;
use crate::{Error, ErrorKind};

#[cfg(any(test, windows, target_os = "linux", target_os = "macos"))]
const DNS_PORT: u16 = 53;
#[cfg(any(test, windows, target_os = "macos"))]
const DEFAULT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(1);
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
const DEFAULT_ATTEMPTS: u8 = 3;
pub(super) const MAX_SEARCH_SUFFIXES: usize = 6;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct DiscoveredResolverConfig {
    pub(super) nameservers: Vec<SocketAddr>,
    pub(super) search_suffixes: Vec<String>,
    pub(super) attempt_timeout: Duration,
    pub(super) attempts: u8,
}

pub(super) fn discover() -> Result<DiscoveredResolverConfig, Error> {
    platform::discover()
}

pub(super) fn normalize_search_suffixes(
    suffixes: impl IntoIterator<Item = impl AsRef<str>>,
) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for suffix in suffixes {
        let Ok(parsed) = normalize_dns_name(suffix.as_ref().trim()) else {
            continue;
        };
        if parsed.identity.is_empty() || !seen.insert(parsed.identity.clone()) {
            continue;
        }
        normalized.push(parsed.identity);
        if normalized.len() == MAX_SEARCH_SUFFIXES {
            break;
        }
    }
    normalized
}

/// FQ-10 Windows suffix policy: a filtered computer `SearchList` is complete; otherwise adapter
/// suffixes in nameserver rank, then computer `Domain`. No devolution.
#[cfg(any(test, all(windows, feature = "resolver")))]
fn assemble_windows_search_suffixes(
    search_list: impl IntoIterator<Item = impl AsRef<str>>,
    adapter_suffixes: impl IntoIterator<Item = impl AsRef<str>>,
    computer_domain: Option<&str>,
) -> Vec<String> {
    let from_list = normalize_search_suffixes(search_list);
    if !from_list.is_empty() {
        return from_list;
    }
    let mut raw = adapter_suffixes
        .into_iter()
        .map(|suffix| suffix.as_ref().to_owned())
        .collect::<Vec<_>>();
    if let Some(domain) = computer_domain {
        raw.push(domain.to_owned());
    }
    normalize_search_suffixes(raw)
}

#[cfg(any(test, windows, target_os = "linux", target_os = "macos"))]
fn finish(
    nameservers: impl IntoIterator<Item = SocketAddr>,
    search_suffixes: impl IntoIterator<Item = impl AsRef<str>>,
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
        search_suffixes: normalize_search_suffixes(search_suffixes),
        attempt_timeout,
        attempts,
    })
}

#[cfg(windows)]
mod platform {
    use ipconfig::OperStatus;
    #[cfg(feature = "resolver")]
    use windows_registry::LOCAL_MACHINE;

    use super::*;

    #[cfg(feature = "resolver")]
    struct RankedAdapter {
        metric: u32,
        adapter_order: usize,
        name: String,
    }

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
        #[cfg(feature = "resolver")]
        let mut ranked_adapters = Vec::new();
        let mut ranked_servers = Vec::new();
        for (adapter_order, adapter) in adapters.into_iter().enumerate() {
            if adapter.oper_status() != OperStatus::IfOperStatusUp {
                continue;
            }
            #[cfg(feature = "resolver")]
            ranked_adapters.push(RankedAdapter {
                metric: adapter.ipv4_metric().min(adapter.ipv6_metric()),
                adapter_order,
                name: adapter.adapter_name().to_owned(),
            });
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
                ranked_servers.push(RankedServer {
                    metric,
                    adapter_order,
                    server_order,
                    address,
                });
            }
        }
        ranked_servers
            .sort_by_key(|server| (server.metric, server.adapter_order, server.server_order));
        #[cfg(feature = "resolver")]
        ranked_adapters.sort_by_key(|adapter| (adapter.metric, adapter.adapter_order));
        #[cfg(feature = "resolver")]
        let search_suffixes = windows_search_suffixes(&ranked_adapters);
        #[cfg(not(feature = "resolver"))]
        let search_suffixes = std::iter::empty::<&'static str>();
        finish(
            ranked_servers.into_iter().map(|server| server.address),
            search_suffixes,
            DEFAULT_ATTEMPT_TIMEOUT,
            DEFAULT_ATTEMPTS,
        )
    }

    #[cfg(feature = "resolver")]
    fn windows_search_suffixes(adapters: &[RankedAdapter]) -> Vec<String> {
        let parameters = LOCAL_MACHINE
            .open(r"SYSTEM\CurrentControlSet\Services\Tcpip\Parameters")
            .ok();
        let search_list = parameters
            .as_ref()
            .and_then(|key| key.get_string("SearchList").ok())
            .unwrap_or_default();
        let adapter_suffixes = adapters
            .iter()
            .filter_map(|adapter| interface_dns_suffix(&adapter.name));
        let computer_domain = parameters
            .as_ref()
            .and_then(|key| registry_nonempty(key, "Domain"));
        assemble_windows_search_suffixes(
            search_list.split(','),
            adapter_suffixes,
            computer_domain.as_deref(),
        )
    }

    #[cfg(feature = "resolver")]
    fn interface_dns_suffix(adapter_name: &str) -> Option<String> {
        let key = LOCAL_MACHINE
            .open(format!(
                r"SYSTEM\CurrentControlSet\Services\Tcpip\Parameters\Interfaces\{adapter_name}"
            ))
            .ok()?;
        registry_nonempty(&key, "Domain").or_else(|| registry_nonempty(&key, "DhcpDomain"))
    }

    #[cfg(feature = "resolver")]
    fn registry_nonempty(key: &windows_registry::Key, name: &str) -> Option<String> {
        key.get_string(name).ok().and_then(|value| {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        })
    }
}

#[cfg(target_os = "linux")]
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
        suffixes_and_nameservers_from_resolv_bytes(&bytes)
    }

    pub(super) fn suffixes_and_nameservers_from_resolv_bytes(
        bytes: &[u8],
    ) -> Result<DiscoveredResolverConfig, Error> {
        let config = resolv_conf::Config::parse(bytes).map_err(|error| {
            Error::new(
                ErrorKind::Internal,
                format!("system DNS configuration could not be parsed: {error}"),
            )
        })?;
        #[cfg(feature = "resolver")]
        let search_suffixes = config
            .get_last_search_or_domain()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        #[cfg(not(feature = "resolver"))]
        let search_suffixes = std::iter::empty::<&'static str>();
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
        finish(nameservers, search_suffixes, timeout, attempts)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    pub(super) fn discover() -> Result<DiscoveredResolverConfig, Error> {
        let config = nbreq_darwin::discover_default_resolver().map_err(|error| {
            Error::new(
                ErrorKind::Unsupported,
                format!("macOS resolver configuration is not supported: {error}"),
            )
        })?;
        let nameservers = macos_nameservers(&config.server_addresses, config.server_port)?;
        let attempt_timeout = macos_attempt_timeout(config.server_timeout)?;
        #[cfg(feature = "resolver")]
        let search_suffixes = config.search_domains;
        #[cfg(not(feature = "resolver"))]
        let search_suffixes = std::iter::empty::<&'static str>();
        finish(
            nameservers,
            search_suffixes,
            attempt_timeout,
            DEFAULT_ATTEMPTS,
        )
    }
}

#[cfg(all(unix, not(target_os = "linux"), not(target_os = "macos")))]
mod platform {
    use super::*;

    pub(super) fn discover() -> Result<DiscoveredResolverConfig, Error> {
        Err(unverified_unix_system_discovery_unsupported())
    }
}

#[cfg(not(any(windows, unix)))]
mod platform {
    use super::*;

    pub(super) fn discover() -> Result<DiscoveredResolverConfig, Error> {
        Err(Error::new(
            ErrorKind::Unsupported,
            "system DNS discovery is supported only on Windows and Linux targets",
        ))
    }
}

#[cfg(any(test, target_os = "macos"))]
fn macos_nameservers(
    addresses: &[String],
    configured_port: Option<i64>,
) -> Result<Vec<SocketAddr>, Error> {
    let port = match configured_port {
        None => DNS_PORT,
        Some(port) => u16::try_from(port)
            .ok()
            .filter(|port| *port != 0)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Unsupported,
                    "macOS reported an invalid DNS server port",
                )
            })?,
    };
    addresses
        .iter()
        .map(|address| {
            if address.contains('%') {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "macOS reported a scoped DNS server address that NBReq cannot yet represent",
                ));
            }
            let address = address.parse::<IpAddr>().map_err(|_| {
                Error::new(
                    ErrorKind::Unsupported,
                    "macOS reported a malformed DNS server address",
                )
            })?;
            if address.is_unspecified() {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "macOS reported an unusable DNS server address",
                ));
            }
            Ok(SocketAddr::new(address, port))
        })
        .collect()
}

#[cfg(any(test, target_os = "macos"))]
fn macos_attempt_timeout(configured_timeout: Option<i64>) -> Result<Duration, Error> {
    let seconds = match configured_timeout {
        None => return Ok(DEFAULT_ATTEMPT_TIMEOUT),
        Some(seconds) if seconds > 0 => u64::try_from(seconds).unwrap_or(u64::MAX).min(5),
        Some(_) => {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "macOS reported an invalid DNS server timeout",
            ));
        }
    };
    Ok(Duration::from_secs(seconds))
}

#[cfg(any(test, all(unix, not(target_os = "linux"), not(target_os = "macos"))))]
fn unverified_unix_system_discovery_unsupported() -> Error {
    Error::new(
        ErrorKind::Unsupported,
        "system DNS discovery is supported on Windows, Linux, and bounded ordinary macOS configurations only; this Unix target is not verified and does not inherit Linux resolv.conf semantics",
    )
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
            ["Corp.TEST.", "corp.test", "invalid_name", "lab.test"],
            Duration::from_secs(2),
            4,
        )
        .expect("usable system configuration");
        assert_eq!(result.nameservers.len(), 2);
        assert_eq!(result.nameservers[0], first);
        assert_eq!(
            result.search_suffixes,
            vec!["corp.test".to_owned(), "lab.test".to_owned()]
        );
        assert_eq!(result.attempt_timeout, Duration::from_secs(2));
        assert_eq!(result.attempts, 4);
    }

    #[test]
    fn search_suffixes_are_bounded_normalized_and_skip_invalid_entries() {
        let suffixes = normalize_search_suffixes([
            " ",
            ".",
            "EXAMPLE.com.",
            "example.com",
            "bad_label",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "one.test",
            "two.test",
            "three.test",
            "four.test",
            "five.test",
            "six.test",
            "seven.test",
        ]);
        assert_eq!(
            suffixes,
            vec![
                "example.com".to_owned(),
                "one.test".to_owned(),
                "two.test".to_owned(),
                "three.test".to_owned(),
                "four.test".to_owned(),
                "five.test".to_owned(),
            ]
        );
        assert_eq!(suffixes.len(), MAX_SEARCH_SUFFIXES);
    }

    #[test]
    fn windows_search_list_is_complete_and_otherwise_uses_ranked_adapters_then_domain() {
        assert_eq!(
            assemble_windows_search_suffixes(
                ["Corp.TEST.", "lab.test", "corp.test"],
                ["adapter.test"],
                Some("computer.test"),
            ),
            vec!["corp.test".to_owned(), "lab.test".to_owned()]
        );
        assert_eq!(
            assemble_windows_search_suffixes(
                [" ", "invalid_name"],
                ["Wifi.TEST.", "lab.test", "wifi.test"],
                Some("corp.example.com"),
            ),
            vec![
                "wifi.test".to_owned(),
                "lab.test".to_owned(),
                "corp.example.com".to_owned()
            ]
        );
        assert_eq!(
            assemble_windows_search_suffixes([] as [&str; 0], [] as [&str; 0], None),
            Vec::<String>::new()
        );
    }

    #[test]
    fn macos_server_fields_are_bounded_and_scoped_addresses_fail_closed() {
        assert_eq!(
            macos_nameservers(&["192.0.2.53".to_owned()], None).expect("default port"),
            ["192.0.2.53:53".parse().expect("fixture socket")]
        );
        assert_eq!(
            macos_nameservers(&["2001:db8::53".to_owned()], Some(5353)).expect("configured port"),
            ["[2001:db8::53]:5353".parse().expect("fixture socket")]
        );
        for invalid in [Some(0), Some(-1), Some(i64::from(u16::MAX) + 1)] {
            let error = macos_nameservers(&["192.0.2.53".to_owned()], invalid)
                .expect_err("invalid port must fail closed");
            assert_eq!(error.kind(), ErrorKind::Unsupported);
        }
        let error = macos_nameservers(&["fe80::1%en0".to_owned()], None)
            .expect_err("scoped address must fail closed");
        assert_eq!(error.kind(), ErrorKind::Unsupported);

        assert_eq!(
            macos_attempt_timeout(None).expect("default timeout"),
            Duration::from_secs(1)
        );
        assert_eq!(
            macos_attempt_timeout(Some(20)).expect("bounded timeout"),
            Duration::from_secs(5)
        );
        assert_eq!(
            macos_attempt_timeout(Some(0))
                .expect_err("zero timeout must fail closed")
                .kind(),
            ErrorKind::Unsupported
        );
    }

    #[test]
    fn unverified_unix_system_discovery_fails_closed_without_linux_semantics() {
        let error = unverified_unix_system_discovery_unsupported();
        assert_eq!(error.kind(), ErrorKind::Unsupported);
        assert!(error.message().contains("bounded ordinary macOS"));
        assert!(
            error
                .message()
                .contains("does not inherit Linux resolv.conf semantics")
        );
    }

    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    #[test]
    fn current_platform_reports_at_least_one_system_nameserver() {
        let discovered = discover().expect("this supported test host must have system DNS");
        assert!(!discovered.nameservers.is_empty());
        assert!(discovered.search_suffixes.len() <= MAX_SEARCH_SUFFIXES);
    }

    #[test]
    #[cfg(all(
        not(feature = "resolver"),
        any(windows, target_os = "linux", target_os = "macos")
    ))]
    fn native_only_system_discovery_omits_public_search_suffixes() {
        let discovered = discover().expect("this supported test host must have system DNS");
        assert!(discovered.search_suffixes.is_empty());
    }

    #[cfg(all(unix, not(target_os = "linux"), not(target_os = "macos")))]
    #[test]
    fn unverified_unix_ordinary_system_discovery_is_unsupported() {
        let error = discover().expect_err("unverified Unix must not inherit Linux discovery");
        assert_eq!(error.kind(), ErrorKind::Unsupported);
        assert!(
            error
                .message()
                .contains("does not inherit Linux resolv.conf semantics")
        );
    }

    #[cfg(all(target_os = "linux", feature = "resolver"))]
    #[test]
    fn linux_resolv_conf_uses_the_last_search_or_domain_directive() {
        let parsed = platform::suffixes_and_nameservers_from_resolv_bytes(
            b"nameserver 192.0.2.53\ndomain ignored.test\nsearch Corp.TEST lab.test\n",
        )
        .expect("fixture resolv.conf must parse");
        assert_eq!(
            parsed.nameservers,
            vec!["192.0.2.53:53".parse().expect("fixture nameserver")]
        );
        assert_eq!(
            parsed.search_suffixes,
            vec!["corp.test".to_owned(), "lab.test".to_owned()]
        );
    }

    #[cfg(all(target_os = "linux", not(feature = "resolver")))]
    #[test]
    fn linux_resolv_conf_native_only_keeps_nameservers_and_omits_suffixes() {
        let parsed = platform::suffixes_and_nameservers_from_resolv_bytes(
            b"nameserver 192.0.2.53\ndomain ignored.test\nsearch Corp.TEST lab.test\n",
        )
        .expect("fixture resolv.conf must parse");
        assert_eq!(
            parsed.nameservers,
            vec!["192.0.2.53:53".parse().expect("fixture nameserver")]
        );
        assert!(parsed.search_suffixes.is_empty());
    }

    #[cfg(any(windows, target_os = "linux"))]
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
