//! Narrow macOS System Configuration reader for NBReq.
//!
//! The main crate forbids unsafe code. Core Foundation dictionaries expose untyped retained
//! values, so the small conversion boundary lives here and returns only owned Rust values.

#![cfg(target_os = "macos")]
#![deny(unsafe_op_in_unsafe_fn)]

use std::error::Error as StdError;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType, ToVoid};
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use system_configuration::dynamic_store::{SCDynamicStore, SCDynamicStoreBuilder};

const GLOBAL_IPV4: &str = "State:/Network/Global/IPv4";
const GLOBAL_IPV6: &str = "State:/Network/Global/IPv6";
const GLOBAL_DNS: &str = "State:/Network/Global/DNS";
const SERVICE_DNS_PATTERN: &str = "State:/Network/Service/.*/DNS";
const INTERFACE_DNS_PATTERN: &str = "State:/Network/Interface/.*/DNS";
const RESOLVER_DIRECTORY: &str = "/etc/resolver";

/// An ordinary default resolver view accepted by NBReq's initial Darwin contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolverConfig {
    /// Server address strings in System Configuration order.
    pub server_addresses: Vec<String>,
    /// Search domains used only by NBReq's opt-in public Resolver expansion.
    pub search_domains: Vec<String>,
    /// Optional DNS server port. NBReq validates and defaults this to 53.
    pub server_port: Option<i64>,
    /// Optional per-attempt timeout in seconds. NBReq bounds this value.
    pub server_timeout: Option<i64>,
}

/// A payload-free platform discovery error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscoveryError {
    message: &'static str,
}

impl DiscoveryError {
    const fn new(message: &'static str) -> Self {
        Self { message }
    }
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl StdError for DiscoveryError {}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct DnsDictionary {
    server_addresses: Vec<String>,
    search_domains: Vec<String>,
    domain_name: Option<String>,
    supplemental_match_domains: Vec<String>,
    has_supplemental_no_search: bool,
    server_port: Option<i64>,
    server_timeout: Option<i64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ResolverSnapshot {
    primary_v4: Option<String>,
    primary_v6: Option<String>,
    services: Vec<(String, DnsDictionary)>,
    interface_dns_count: usize,
    global: Option<DnsDictionary>,
}

/// Reads and validates the current ordinary default DNS view.
///
/// Multi-service, split, supplemental, interface-scoped, conflicting-primary, and malformed
/// states fail closed rather than being flattened into one global server list.
pub fn discover_default_resolver() -> Result<ResolverConfig, DiscoveryError> {
    reject_resolver_directory(Path::new(RESOLVER_DIRECTORY))?;
    let store = SCDynamicStoreBuilder::new("com.caverock.nbreq.dns-discovery")
        .build()
        .ok_or_else(|| {
            DiscoveryError::new("could not open the macOS System Configuration store")
        })?;
    select_default(snapshot(&store)?)
}

fn reject_resolver_directory(path: &Path) -> Result<(), DiscoveryError> {
    let mut entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(_) => {
            return Err(DiscoveryError::new(
                "could not inspect the macOS supplemental resolver directory",
            ));
        }
    };
    match entries.next() {
        None => Ok(()),
        Some(Ok(_)) => Err(DiscoveryError::new(
            "macOS /etc/resolver supplemental DNS routing is not yet represented",
        )),
        Some(Err(_)) => Err(DiscoveryError::new(
            "could not inspect the macOS supplemental resolver directory",
        )),
    }
}

fn snapshot(store: &SCDynamicStore) -> Result<ResolverSnapshot, DiscoveryError> {
    let primary_v4 = optional_dictionary(store, GLOBAL_IPV4)?
        .as_ref()
        .map(|dictionary| optional_string_field(dictionary, "PrimaryService"))
        .transpose()?
        .flatten();
    let primary_v6 = optional_dictionary(store, GLOBAL_IPV6)?
        .as_ref()
        .map(|dictionary| optional_string_field(dictionary, "PrimaryService"))
        .transpose()?
        .flatten();
    let service_keys = keys(store, SERVICE_DNS_PATTERN)?;
    let mut services = Vec::with_capacity(service_keys.len());
    for key in service_keys {
        let service = key
            .strip_prefix("State:/Network/Service/")
            .and_then(|value| value.strip_suffix("/DNS"))
            .filter(|value| !value.is_empty())
            .ok_or_else(|| DiscoveryError::new("macOS returned a malformed service DNS key"))?;
        let dictionary = required_dictionary(store, &key)?;
        services.push((service.to_owned(), parse_dns_dictionary(&dictionary)?));
    }
    let interface_dns_count = keys(store, INTERFACE_DNS_PATTERN)?.len();
    let global = optional_dictionary(store, GLOBAL_DNS)?
        .as_ref()
        .map(parse_dns_dictionary)
        .transpose()?;
    Ok(ResolverSnapshot {
        primary_v4,
        primary_v6,
        services,
        interface_dns_count,
        global,
    })
}

fn select_default(snapshot: ResolverSnapshot) -> Result<ResolverConfig, DiscoveryError> {
    let primary = match (&snapshot.primary_v4, &snapshot.primary_v6) {
        (Some(v4), Some(v6)) if v4 != v6 => {
            return Err(DiscoveryError::new(
                "macOS reported conflicting IPv4 and IPv6 primary services",
            ));
        }
        (Some(primary), _) | (_, Some(primary)) => primary,
        (None, None) => {
            return Err(DiscoveryError::new(
                "macOS reported no primary network service",
            ));
        }
    };
    if snapshot.interface_dns_count != 0 {
        return Err(DiscoveryError::new(
            "macOS interface-scoped DNS routing is not yet represented",
        ));
    }
    if snapshot.services.len() != 1 || snapshot.services[0].0 != *primary {
        return Err(DiscoveryError::new(
            "macOS multi-service or non-primary DNS routing is not yet represented",
        ));
    }
    let service = &snapshot.services[0].1;
    let global = snapshot
        .global
        .as_ref()
        .ok_or_else(|| DiscoveryError::new("macOS reported no global DNS state"))?;
    if !service.supplemental_match_domains.is_empty()
        || service.has_supplemental_no_search
        || !global.supplemental_match_domains.is_empty()
        || global.has_supplemental_no_search
    {
        return Err(DiscoveryError::new(
            "macOS supplemental DNS routing is not yet represented",
        ));
    }
    if service.server_addresses != global.server_addresses
        || effective_port(service.server_port) != effective_port(global.server_port)
        || effective_timeout(service.server_timeout) != effective_timeout(global.server_timeout)
        || effective_search_domains(service) != effective_search_domains(global)
    {
        return Err(DiscoveryError::new(
            "macOS primary-service and global DNS state disagree",
        ));
    }
    if service.server_addresses.is_empty() {
        return Err(DiscoveryError::new(
            "macOS reported no default DNS server addresses",
        ));
    }
    Ok(ResolverConfig {
        server_addresses: service.server_addresses.clone(),
        search_domains: effective_search_domains(service),
        server_port: service.server_port,
        server_timeout: service.server_timeout,
    })
}

fn effective_port(port: Option<i64>) -> i64 {
    port.unwrap_or(53)
}

fn effective_timeout(timeout: Option<i64>) -> i64 {
    timeout.unwrap_or(1)
}

fn effective_search_domains(dictionary: &DnsDictionary) -> Vec<String> {
    if !dictionary.search_domains.is_empty() {
        dictionary.search_domains.clone()
    } else {
        dictionary.domain_name.iter().cloned().collect()
    }
}

fn parse_dns_dictionary(dictionary: &CFDictionary) -> Result<DnsDictionary, DiscoveryError> {
    Ok(DnsDictionary {
        server_addresses: string_array_field(dictionary, "ServerAddresses")?,
        search_domains: string_array_field(dictionary, "SearchDomains")?,
        domain_name: optional_string_field(dictionary, "DomainName")?,
        supplemental_match_domains: string_array_field(dictionary, "SupplementalMatchDomains")?,
        has_supplemental_no_search: value(dictionary, "SupplementalMatchDomainsNoSearch").is_some(),
        server_port: number_field(dictionary, "ServerPort")?,
        server_timeout: number_field(dictionary, "ServerTimeout")?,
    })
}

fn keys(store: &SCDynamicStore, pattern: &str) -> Result<Vec<String>, DiscoveryError> {
    let keys = store
        .get_keys(pattern)
        .ok_or_else(|| DiscoveryError::new("macOS DNS key enumeration failed"))?;
    Ok(keys.iter().map(|key| key.to_string()).collect())
}

fn required_dictionary(store: &SCDynamicStore, key: &str) -> Result<CFDictionary, DiscoveryError> {
    optional_dictionary(store, key)?
        .ok_or_else(|| DiscoveryError::new("macOS DNS state disappeared during discovery"))
}

fn optional_dictionary(
    store: &SCDynamicStore,
    key: &str,
) -> Result<Option<CFDictionary>, DiscoveryError> {
    store
        .get(key)
        .map(|value| {
            value
                .downcast_into::<CFDictionary>()
                .ok_or_else(|| DiscoveryError::new("macOS returned non-dictionary network state"))
        })
        .transpose()
}

fn value(dictionary: &CFDictionary, key: &str) -> Option<CFType> {
    let key = CFString::new(key);
    dictionary.find(key.to_void()).map(|pointer| {
        // SAFETY: CFDictionary owns and retains this value for the duration of the lookup. The
        // get-rule wrapper increments the retain count and does not outlive `dictionary` via a
        // borrowed pointer.
        unsafe { CFType::wrap_under_get_rule(*pointer) }
    })
}

fn optional_string_field(
    dictionary: &CFDictionary,
    key: &str,
) -> Result<Option<String>, DiscoveryError> {
    let Some(value) = value(dictionary, key) else {
        return Ok(None);
    };
    value
        .downcast_into::<CFString>()
        .map(|value| Some(value.to_string()))
        .ok_or_else(|| DiscoveryError::new("macOS returned a non-string DNS field"))
}

fn string_array_field(dictionary: &CFDictionary, key: &str) -> Result<Vec<String>, DiscoveryError> {
    let Some(value) = value(dictionary, key) else {
        return Ok(Vec::new());
    };
    let array = value
        .downcast_into::<CFArray>()
        .ok_or_else(|| DiscoveryError::new("macOS returned a non-array DNS field"))?;
    let mut values = Vec::with_capacity(array.len() as usize);
    for pointer in &array {
        // SAFETY: CFArray retains each element while `array` is alive. The get-rule wrapper
        // increments the retain count before the local owned CFType is downcast.
        let value = unsafe { CFType::wrap_under_get_rule(*pointer) };
        let value = value
            .downcast_into::<CFString>()
            .ok_or_else(|| DiscoveryError::new("macOS returned a non-string DNS array item"))?;
        values.push(value.to_string());
    }
    Ok(values)
}

fn number_field(dictionary: &CFDictionary, key: &str) -> Result<Option<i64>, DiscoveryError> {
    let Some(value) = value(dictionary, key) else {
        return Ok(None);
    };
    let number = value
        .downcast_into::<CFNumber>()
        .ok_or_else(|| DiscoveryError::new("macOS returned a non-number DNS field"))?;
    number
        .to_i64()
        .map(Some)
        .ok_or_else(|| DiscoveryError::new("macOS returned an unrepresentable DNS number"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

    fn temporary_directory() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "nbreq-darwin-resolver-test-{}-{}",
            std::process::id(),
            TEMP_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn ordinary() -> ResolverSnapshot {
        let dns = DnsDictionary {
            server_addresses: vec!["192.0.2.53".to_owned()],
            search_domains: vec!["example.test".to_owned()],
            server_timeout: Some(2),
            ..DnsDictionary::default()
        };
        ResolverSnapshot {
            primary_v4: Some("primary".to_owned()),
            primary_v6: Some("primary".to_owned()),
            services: vec![("primary".to_owned(), dns.clone())],
            global: Some(dns),
            ..ResolverSnapshot::default()
        }
    }

    #[test]
    fn ordinary_primary_and_global_view_is_accepted() {
        let selected = select_default(ordinary()).expect("ordinary resolver must be accepted");
        assert_eq!(selected.server_addresses, ["192.0.2.53"]);
        assert_eq!(selected.search_domains, ["example.test"]);
        assert_eq!(selected.server_timeout, Some(2));
    }

    #[test]
    fn domain_name_is_search_fallback_only() {
        let mut snapshot = ordinary();
        snapshot.services[0].1.search_domains.clear();
        snapshot.services[0].1.domain_name = Some("fallback.test".to_owned());
        let global = snapshot.global.as_mut().expect("global fixture");
        global.search_domains.clear();
        global.domain_name = Some("fallback.test".to_owned());
        let selected = select_default(snapshot).expect("fallback domain must be accepted");
        assert_eq!(selected.search_domains, ["fallback.test"]);
    }

    #[test]
    fn complex_routing_states_fail_closed() {
        let mut conflicting = ordinary();
        conflicting.primary_v6 = Some("other".to_owned());
        assert!(select_default(conflicting).is_err());

        let mut additional = ordinary();
        additional
            .services
            .push(("vpn".to_owned(), DnsDictionary::default()));
        assert!(select_default(additional).is_err());

        let mut interface = ordinary();
        interface.interface_dns_count = 1;
        assert!(select_default(interface).is_err());

        let mut supplemental = ordinary();
        supplemental.services[0].1.supplemental_match_domains = vec!["corp.test".to_owned()];
        assert!(select_default(supplemental).is_err());

        let mut no_search = ordinary();
        no_search.services[0].1.has_supplemental_no_search = true;
        assert!(select_default(no_search).is_err());
    }

    #[test]
    fn primary_and_global_disagreement_fails_closed() {
        let mut snapshot = ordinary();
        snapshot
            .global
            .as_mut()
            .expect("global fixture")
            .server_addresses = vec!["192.0.2.54".to_owned()];
        assert!(select_default(snapshot).is_err());

        let mut timeout = ordinary();
        timeout
            .global
            .as_mut()
            .expect("global fixture")
            .server_timeout = Some(3);
        assert!(select_default(timeout).is_err());
    }

    #[test]
    fn supplemental_resolver_directory_fails_closed() {
        let root = temporary_directory();
        let resolver = root.join("resolver");
        reject_resolver_directory(&resolver).expect("a missing directory has no routes");

        fs::create_dir_all(&resolver).expect("create empty resolver fixture");
        reject_resolver_directory(&resolver).expect("an empty directory has no routes");

        fs::write(resolver.join("corp.test"), "nameserver 192.0.2.53\n")
            .expect("write supplemental resolver fixture");
        let error = reject_resolver_directory(&resolver)
            .expect_err("a supplemental resolver entry must fail closed");
        assert_eq!(
            error.to_string(),
            "macOS /etc/resolver supplemental DNS routing is not yet represented"
        );

        fs::remove_dir_all(root).expect("remove resolver fixture");
    }
}
