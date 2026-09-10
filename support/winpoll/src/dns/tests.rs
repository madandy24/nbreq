use super::*;

const BASE: usize = 0x10000;
const DNS: usize = 901;
const DNS6: usize = 1001;
const SOCKET: usize = 1101;
const SOCKET6: usize = 1201;
const NAME: usize = 1401;

struct Fixture(Vec<u8>);

impl Fixture {
    fn new() -> Self {
        let mut fixture = Self(vec![0; 2048]);
        fixture.u32(0, size_of::<Adapter>() as u32);
        fixture.pointer(offset_of!(Adapter, AdapterName), BASE + NAME);
        fixture.pointer(offset_of!(Adapter, FirstDnsServerAddress), BASE + DNS);
        fixture.u32(offset_of!(Adapter, OperStatus), 1);
        fixture.u32(offset_of!(Adapter, Ipv4Metric), 33);
        fixture.u32(offset_of!(Adapter, Ipv6Metric), 5);
        fixture.u32(offset_of!(Adapter, Ipv6IfIndex), 17);
        fixture.0[NAME..NAME + 5].copy_from_slice(b"eth0\0");
        for (node, socket, length) in [
            (DNS, SOCKET, size_of::<SOCKADDR_IN>()),
            (DNS6, SOCKET6, size_of::<SOCKADDR_IN6>()),
        ] {
            fixture.u32(node, size_of::<Server>() as u32);
            fixture.pointer(
                node + offset_of!(Server, Address) + offset_of!(SOCKET_ADDRESS, lpSockaddr),
                BASE + socket,
            );
            fixture.u32(
                node + offset_of!(Server, Address) + offset_of!(SOCKET_ADDRESS, iSockaddrLength),
                length as u32,
            );
        }
        fixture.pointer(DNS + offset_of!(Server, Next), BASE + DNS6);
        fixture.0[SOCKET..SOCKET + 2].copy_from_slice(&AF_INET.to_le_bytes());
        fixture.0[SOCKET + offset_of!(SOCKADDR_IN, sin_port)
            ..SOCKET + offset_of!(SOCKADDR_IN, sin_port) + 2]
            .copy_from_slice(&53_u16.to_be_bytes());
        fixture.0[SOCKET + offset_of!(SOCKADDR_IN, sin_addr)
            ..SOCKET + offset_of!(SOCKADDR_IN, sin_addr) + 4]
            .copy_from_slice(&[192, 0, 2, 53]);
        fixture.0[SOCKET6..SOCKET6 + 2].copy_from_slice(&AF_INET6.to_le_bytes());
        fixture.0[SOCKET6 + offset_of!(SOCKADDR_IN6, sin6_port)
            ..SOCKET6 + offset_of!(SOCKADDR_IN6, sin6_port) + 2]
            .copy_from_slice(&53_u16.to_be_bytes());
        fixture.0[SOCKET6 + offset_of!(SOCKADDR_IN6, sin6_addr)
            ..SOCKET6 + offset_of!(SOCKADDR_IN6, sin6_addr) + 16]
            .copy_from_slice(
                &"fe80::53"
                    .parse::<Ipv6Addr>()
                    .expect("fixture IPv6")
                    .octets(),
            );
        fixture.u32(SOCKET6 + offset_of!(SOCKADDR_IN6, Anonymous), 29);
        fixture
    }

    fn u32(&mut self, offset: usize, value: u32) {
        self.0[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn pointer(&mut self, offset: usize, value: usize) {
        self.0[offset..offset + size_of::<usize>()].copy_from_slice(&value.to_le_bytes());
    }

    fn parse(&self) -> io::Result<Vec<DnsAdapter>> {
        parse(&self.0, BASE)
    }
}

#[test]
fn odd_addressed_descriptive_strings_are_never_decoded() {
    let mut fixture = Fixture::new();
    let expected = fixture.parse().expect("baseline DNS fields");
    // Deliberately odd UTF-16 address, containing an unpaired surrogate and no terminator.
    fixture.0[1501..].fill(0xd8);
    fixture.pointer(offset_of!(Adapter, Description), BASE + 1501);
    fixture.pointer(offset_of!(Adapter, FriendlyName), BASE + 1503);
    fixture.pointer(offset_of!(Adapter, DnsSuffix), BASE + 1505);
    assert_eq!(
        fixture.parse().expect("unused strings must not affect DNS"),
        expected
    );
}

#[test]
fn null_and_out_of_bounds_unused_fields_are_not_followed() {
    for pointer in [0, BASE - 1, BASE + 2048, usize::MAX] {
        let mut fixture = Fixture::new();
        let expected = fixture.parse().expect("baseline");
        for field in [
            offset_of!(Adapter, Description),
            offset_of!(Adapter, FriendlyName),
            offset_of!(Adapter, DnsSuffix),
            offset_of!(Adapter, FirstUnicastAddress),
            offset_of!(Adapter, FirstPrefix),
        ] {
            fixture.pointer(field, pointer);
        }
        assert_eq!(fixture.parse().expect("unused metadata ignored"), expected);
    }
}

#[test]
fn unaligned_records_and_socket_data_preserve_order_metrics_and_explicit_scope() {
    let mut fixture = Fixture::new();
    // A second adapter begins at an odd offset. Both DNS nodes and sockets are already odd.
    let second = 401;
    let header = fixture.0[..size_of::<Adapter>()].to_vec();
    fixture.0[second..second + header.len()].copy_from_slice(&header);
    fixture.pointer(offset_of!(Adapter, Next), BASE + second);
    fixture.u32(second + offset_of!(Adapter, Ipv4Metric), 44);
    let adapters = fixture.parse().expect("unaligned API layout");
    assert_eq!(adapters.len(), 2);
    assert_eq!(adapters[0].name, "eth0");
    assert!(adapters[0].up);
    assert_eq!(
        (
            adapters[0].ipv4_metric,
            adapters[0].ipv6_metric,
            adapters[0].ipv6_if_index
        ),
        (33, 5, 17)
    );
    assert_eq!(adapters[1].ipv4_metric, 44);
    assert_eq!(
        adapters[0].dns_servers,
        [
            "192.0.2.53:53".parse().expect("IPv4"),
            "[fe80::53%29]:53".parse().expect("scoped IPv6")
        ]
    );
}

#[test]
fn unavailable_and_unknown_operational_statuses_are_not_up() {
    for status in [0, 2, 3, 4, 5, 6, 7, 999, u32::MAX] {
        let mut fixture = Fixture::new();
        fixture.u32(offset_of!(Adapter, OperStatus), status);
        assert!(!fixture.parse().expect("unrecognized status is not a panic")[0].up);
    }
}

#[test]
fn no_dns_servers_is_a_valid_adapter_record() {
    let mut fixture = Fixture::new();
    fixture.pointer(offset_of!(Adapter, FirstDnsServerAddress), 0);
    assert!(
        fixture.parse().expect("no DNS configured")[0]
            .dns_servers
            .is_empty()
    );
}

#[test]
fn required_pointers_must_remain_inside_owned_storage() {
    for field in [
        offset_of!(Adapter, AdapterName),
        offset_of!(Adapter, Next),
        offset_of!(Adapter, FirstDnsServerAddress),
        DNS + offset_of!(Server, Address) + offset_of!(SOCKET_ADDRESS, lpSockaddr),
    ] {
        for pointer in [BASE - 1, BASE + 2048, usize::MAX] {
            let mut fixture = Fixture::new();
            fixture.pointer(field, pointer);
            assert_eq!(
                fixture.parse().expect_err("out-of-buffer pointer").kind(),
                io::ErrorKind::InvalidData
            );
        }
    }
    for field in [
        offset_of!(Adapter, AdapterName),
        DNS + offset_of!(Server, Address) + offset_of!(SOCKET_ADDRESS, lpSockaddr),
    ] {
        let mut fixture = Fixture::new();
        fixture.pointer(field, 0);
        assert!(fixture.parse().is_err(), "required pointer may not be null");
    }
}

#[test]
fn declared_record_lengths_must_cover_fields_and_fit_buffer() {
    for length in [
        0,
        (offset_of!(Adapter, Ipv6Metric) + 3) as u32,
        2049,
        u32::MAX,
    ] {
        let mut fixture = Fixture::new();
        fixture.u32(0, length);
        assert!(fixture.parse().is_err(), "adapter length {length}");
    }
    for length in [
        0,
        (offset_of!(Server, Address) + size_of::<SOCKET_ADDRESS>() - 1) as u32,
        2048,
    ] {
        let mut fixture = Fixture::new();
        fixture.u32(DNS, length);
        assert!(fixture.parse().is_err(), "DNS record length {length}");
    }
}

#[test]
fn interface_identifiers_must_be_terminated_and_valid_utf8() {
    let mut fixture = Fixture::new();
    fixture.0[NAME..].fill(b'x');
    assert!(fixture.parse().is_err());
    fixture.0[NAME] = 0xff;
    fixture.0[NAME + 1] = 0;
    assert!(fixture.parse().is_err());
}

#[test]
fn linked_list_cycles_return_errors() {
    for (field, pointer) in [
        (offset_of!(Adapter, Next), BASE),
        (DNS + offset_of!(Server, Next), BASE + DNS),
    ] {
        let mut fixture = Fixture::new();
        fixture.pointer(field, pointer);
        assert!(
            fixture
                .parse()
                .expect_err("cycle")
                .to_string()
                .contains("cycle")
        );
    }
}

#[test]
fn malformed_socket_lengths_and_families_return_errors() {
    for length in [0, 1, 15, 2048, u32::MAX] {
        let mut fixture = Fixture::new();
        fixture.u32(
            DNS + offset_of!(Server, Address) + offset_of!(SOCKET_ADDRESS, iSockaddrLength),
            length,
        );
        assert!(fixture.parse().is_err(), "socket length {length}");
    }
    let mut fixture = Fixture::new();
    fixture.0[SOCKET..SOCKET + 2].copy_from_slice(&999_u16.to_le_bytes());
    assert!(fixture.parse().is_err());
    let mut fixture = Fixture::new();
    fixture.u32(
        DNS6 + offset_of!(Server, Address) + offset_of!(SOCKET_ADDRESS, iSockaddrLength),
        27,
    );
    assert!(fixture.parse().is_err());
}

#[test]
fn buffer_growth_is_bounded_and_os_errors_are_preserved() {
    let mut calls = 0;
    let error = collect(|_, required| {
        calls += 1;
        *required *= 2;
        ERROR_BUFFER_OVERFLOW
    })
    .expect_err("growth limit");
    assert_eq!(calls, 5);
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(
        collect(|_, _| ERROR_BUFFER_OVERFLOW).is_err(),
        "non-growing retry"
    );
    assert!(
        collect(|_, required| {
            *required += 1;
            ERROR_SUCCESS
        })
        .is_err(),
        "oversized success"
    );
    assert!(
        collect(|_, required| {
            *required = 0;
            ERROR_SUCCESS
        })
        .is_err(),
        "empty success"
    );
    assert_eq!(
        collect(|_, _| 5).expect_err("OS failure").raw_os_error(),
        Some(5)
    );
}

#[test]
fn ordinary_windows_adapter_discovery_returns_owned_dns_fields() {
    let found = adapters().expect("this test host must expose adapter configuration");
    assert!(
        found
            .iter()
            .any(|adapter| adapter.up && !adapter.dns_servers.is_empty())
    );
}
