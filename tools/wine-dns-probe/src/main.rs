//! Diagnostic only: inspect owned GetAdaptersAddresses storage before any string conversion.
use std::collections::HashSet;
use std::mem::{align_of, offset_of, size_of};
use windows_sys::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_INCLUDE_PREFIX, GetAdaptersAddresses,
    IP_ADAPTER_ADDRESSES_LH,
};
use windows_sys::Win32::Networking::WinSock::AF_UNSPEC;

fn main() {
    println!(
        "probe pointer_bits={} adapter_size={} adapter_align={} description_offset={} friendly_offset={}",
        usize::BITS,
        size_of::<IP_ADAPTER_ADDRESSES_LH>(),
        align_of::<IP_ADAPTER_ADDRESSES_LH>(),
        offset_of!(IP_ADAPTER_ADDRESSES_LH, Description),
        offset_of!(IP_ADAPTER_ADDRESSES_LH, FriendlyName)
    );
    if std::env::args().any(|arg| arg == "--fixed") {
        let adapters = nbreq_winpoll::dns::adapters().expect("bounded DNS discovery");
        println!(
            "fixed returned {} adapters; {} DNS endpoints",
            adapters.len(),
            adapters.iter().map(|a| a.dns_servers.len()).sum::<usize>()
        );
        return;
    }
    if let Some(url) = std::env::args()
        .nth(1)
        .filter(|arg| arg.starts_with("http://"))
    {
        exercise_nbreq(&url);
        return;
    }
    if std::env::args().any(|arg| arg == "--legacy") {
        println!("legacy ipconfig=0.3.4 widestring=1.2.1: entering get_adapters");
        let adapters = ipconfig::get_adapters().expect("legacy adapter discovery");
        println!("legacy returned {} adapters", adapters.len());
        return;
    }
    let mut required = 16 * 1024_u32;
    for _ in 0..5 {
        assert!(required <= 4 * 1024 * 1024, "unreasonable API buffer size");
        let mut words = vec![0_u64; (required as usize).div_ceil(8)];
        let capacity = words.len() * 8;
        let result = unsafe {
            // Owned, initialized, suitably aligned storage remains fixed throughout the call.
            GetAdaptersAddresses(
                AF_UNSPEC as u32,
                GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_INCLUDE_PREFIX,
                std::ptr::null(),
                words.as_mut_ptr().cast(),
                &mut required,
            )
        };
        if result == ERROR_BUFFER_OVERFLOW {
            continue;
        }
        assert_eq!(result, ERROR_SUCCESS, "GetAdaptersAddresses");
        let bytes = unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), capacity) };
        println!(
            "buffer base={:#x} capacity={} api_size={}",
            bytes.as_ptr() as usize,
            capacity,
            required
        );
        let mut cursor = bytes.as_ptr() as usize;
        let mut seen = HashSet::new();
        while cursor != 0 {
            assert!(seen.insert(cursor), "adapter list cycle");
            let offset = cursor
                .checked_sub(bytes.as_ptr() as usize)
                .expect("adapter before buffer");
            let raw = bytes
                .get(
                    offset
                        ..offset
                            .checked_add(size_of::<IP_ADAPTER_ADDRESSES_LH>())
                            .unwrap(),
                )
                .expect("adapter outside owned buffer");
            let adapter = unsafe {
                raw.as_ptr()
                    .cast::<IP_ADAPTER_ADDRESSES_LH>()
                    .read_unaligned()
            };
            let length = unsafe { adapter.Anonymous1.Anonymous.Length };
            println!(
                "adapter index={} offset={} alignment={} declared_length={} status={} metrics={}/{} ipv6_if={}",
                seen.len() - 1,
                offset,
                cursor % align_of::<IP_ADAPTER_ADDRESSES_LH>(),
                length,
                adapter.OperStatus,
                adapter.Ipv4Metric,
                adapter.Ipv6Metric,
                adapter.Ipv6IfIndex
            );
            assert!(length as usize >= offset_of!(IP_ADAPTER_ADDRESSES_LH, Ipv6Metric) + 4);
            inspect_utf16(bytes, "description", adapter.Description as usize);
            inspect_utf16(bytes, "friendly", adapter.FriendlyName as usize);
            inspect_utf16(bytes, "suffix", adapter.DnsSuffix as usize);
            cursor = adapter.Next as usize;
        }
        println!(
            "inspection complete: {} adapters; no UTF-16 pointers dereferenced",
            seen.len()
        );
        return;
    }
    panic!("adapter list kept growing");
}

fn exercise_nbreq(url: &str) {
    #[cfg(feature = "resolver")]
    use nbreq::{AddressFamily, ResolveRequest};
    use nbreq::{Engine, EngineConfig, Request};
    use std::time::Duration;
    let engine = Engine::new(EngineConfig::spawned()).expect("ordinary nbreq Engine");
    let response = engine
        .client()
        .execute(
            Request::get(url)
                .total_timeout(Duration::from_secs(10))
                .build()
                .expect("HTTP request"),
        )
        .expect("HTTP through ordinary system discovery");
    assert_eq!(response.body(), b"wine-dns-ok");
    println!("ordinary HTTP passed");
    #[cfg(feature = "resolver")]
    {
        let dns = engine
            .resolver()
            .execute(
                ResolveRequest::hostname("example.com")
                    .address_family(AddressFamily::Ipv4)
                    .total_timeout(Duration::from_secs(10))
                    .build()
                    .expect("DNS request"),
            )
            .expect("public resolver through system discovery");
        assert!(!dns.addresses().is_empty());
        println!("ordinary public DNS passed");
    }
    let remote = engine
        .client()
        .execute(
            Request::get("http://example.com/")
                .total_timeout(Duration::from_secs(15))
                .build()
                .expect("hostname HTTP request"),
        )
        .expect("HTTP hostname resolution through system discovery");
    assert!(remote.status() >= 200);
    println!("ordinary hostname HTTP passed status={}", remote.status());
    engine.shutdown().expect("joined Engine shutdown");
    println!("joined shutdown passed");
}

fn inspect_utf16(bytes: &[u8], label: &str, pointer: usize) {
    let offset = pointer.checked_sub(bytes.as_ptr() as usize);
    let tail = offset.and_then(|offset| bytes.get(offset..));
    let units = tail.and_then(|tail| tail.chunks_exact(2).position(|pair| pair == [0, 0]));
    println!(
        "string field={label} pointer={pointer:#x} alignment={} in_bounds={} terminated_units={units:?}",
        pointer % 2,
        tail.is_some()
    );
}
