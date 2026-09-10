//! DNS-relevant adapter fields only. Descriptions, friendly names and other unused linked
//! data are never followed or decoded. In particular, old Wine can return unaligned UTF-16
//! descriptive strings. All parsing below the FFI boundary uses checked byte slices.

use std::collections::HashSet;
use std::io;
use std::mem::{offset_of, size_of};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};

use windows_sys::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_FRIENDLY_NAME, GAA_FLAG_SKIP_MULTICAST,
    GAA_FLAG_SKIP_UNICAST, GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH as Adapter,
    IP_ADAPTER_DNS_SERVER_ADDRESS_XP as Server,
};
use windows_sys::Win32::Networking::WinSock::{
    AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_IN, SOCKADDR_IN6, SOCKET_ADDRESS,
};

/// Owned fields consumed by NBReq's existing ranking and registry suffix policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DnsAdapter {
    /// System interface identifier (AdapterName), not its human-readable friendly name.
    pub name: String,
    /// Whether the interface reports IfOperStatusUp. Other status values are not active.
    pub up: bool,
    pub ipv4_metric: u32,
    pub ipv6_metric: u32,
    pub ipv6_if_index: u32,
    /// OS order, including the explicit IPv6 scope ID. NBReq supplies its DNS port policy.
    pub dns_servers: Vec<SocketAddr>,
}

/// Read adapter DNS configuration. Returned values own their data; no OS pointers escape.
pub fn adapters() -> io::Result<Vec<DnsAdapter>> {
    collect(|words, required| {
        // SAFETY: words is initialized, writable, at least *required bytes long, and aligned
        // for the API's initial structure. It cannot move during the call. The API does not
        // retain it. Subsequent returned records need not have Rust's structure alignment.
        unsafe {
            GetAdaptersAddresses(
                u32::from(AF_UNSPEC),
                GAA_FLAG_SKIP_UNICAST
                    | GAA_FLAG_SKIP_ANYCAST
                    | GAA_FLAG_SKIP_MULTICAST
                    | GAA_FLAG_SKIP_FRIENDLY_NAME,
                std::ptr::null(),
                words.as_mut_ptr().cast(),
                required,
            )
        }
    })
}

fn collect(mut fill: impl FnMut(&mut [u64], &mut u32) -> u32) -> io::Result<Vec<DnsAdapter>> {
    let mut required = 16 * 1024_u32;
    for _ in 0..5 {
        let supplied = required;
        let units = (supplied as usize).div_ceil(size_of::<u64>());
        let mut words = Vec::new();
        words.try_reserve_exact(units).map_err(|_| {
            io::Error::new(
                io::ErrorKind::OutOfMemory,
                "Windows adapter buffer allocation failed",
            )
        })?;
        words.resize(units, 0_u64);
        let result = fill(&mut words, &mut required);
        if result == ERROR_BUFFER_OVERFLOW {
            if required <= supplied {
                return Err(invalid("Windows adapter buffer size did not grow"));
            }
            continue;
        }
        if result != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(result as i32));
        }
        if required == 0 || required > supplied {
            return Err(invalid("Windows adapter success reported an invalid size"));
        }
        // SAFETY: the initialized u64 allocation contains at least `required` bytes. All byte
        // patterns are valid u8 values. `words` stays alive and immutable through parsing.
        let bytes =
            unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), required as usize) };
        return parse(bytes, words.as_ptr() as usize);
    }
    Err(invalid(
        "Windows adapter configuration kept growing during discovery",
    ))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

struct Buffer<'a> {
    bytes: &'a [u8],
    base: usize,
}

impl Buffer<'_> {
    fn bytes(&self, offset: usize, length: usize) -> io::Result<&[u8]> {
        let end = offset
            .checked_add(length)
            .ok_or_else(|| invalid("adapter range overflow"))?;
        self.bytes
            .get(offset..end)
            .ok_or_else(|| invalid("adapter field outside buffer"))
    }

    fn array<const N: usize>(&self, offset: usize) -> io::Result<[u8; N]> {
        self.bytes(offset, N)?
            .try_into()
            .map_err(|_| invalid("invalid adapter field size"))
    }

    fn u32(&self, offset: usize) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.array(offset)?))
    }

    fn pointer(&self, offset: usize) -> io::Result<usize> {
        Ok(usize::from_le_bytes(self.array(offset)?))
    }

    fn offset(&self, pointer: usize, length: usize) -> io::Result<usize> {
        let offset = pointer
            .checked_sub(self.base)
            .ok_or_else(|| invalid("adapter pointer before buffer"))?;
        self.bytes(offset, length)?;
        Ok(offset)
    }

    fn record(&self, pointer: usize, minimum: usize) -> io::Result<usize> {
        let offset = self.offset(pointer, size_of::<u32>())?;
        let length = self.u32(offset)? as usize;
        if length < minimum {
            return Err(invalid("adapter record is too short for required fields"));
        }
        self.bytes(offset, length)?;
        Ok(offset)
    }

    fn name(&self, pointer: usize) -> io::Result<String> {
        let offset = self.offset(pointer, 1)?;
        let tail = &self.bytes[offset..];
        let end = tail
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| invalid("unterminated adapter identifier"))?;
        std::str::from_utf8(&tail[..end])
            .map(str::to_owned)
            .map_err(|_| invalid("invalid adapter identifier encoding"))
    }

    fn servers(&self, mut pointer: usize) -> io::Result<Vec<SocketAddr>> {
        let mut result = Vec::new();
        let mut seen = HashSet::new();
        while pointer != 0 {
            if !seen.insert(pointer) {
                return Err(invalid("cycle in adapter DNS list"));
            }
            let offset = self.record(
                pointer,
                offset_of!(Server, Address) + size_of::<SOCKET_ADDRESS>(),
            )?;
            let address = offset + offset_of!(Server, Address);
            let socket_pointer = self.pointer(address + offset_of!(SOCKET_ADDRESS, lpSockaddr))?;
            let length = self.u32(address + offset_of!(SOCKET_ADDRESS, iSockaddrLength))?;
            // iSockaddrLength is signed. Negative values must never become large slice sizes.
            if length > i32::MAX as u32 {
                return Err(invalid("negative adapter DNS socket length"));
            }
            let socket = self.offset(socket_pointer, length as usize)?;
            if length < 2 {
                return Err(invalid("missing adapter DNS socket family"));
            }
            let family = u16::from_le_bytes(self.array(socket)?);
            let endpoint = match family {
                AF_INET if length as usize >= size_of::<SOCKADDR_IN>() => {
                    let ip = Ipv4Addr::from(
                        self.array::<4>(socket + offset_of!(SOCKADDR_IN, sin_addr))?,
                    );
                    let port =
                        u16::from_be_bytes(self.array(socket + offset_of!(SOCKADDR_IN, sin_port))?);
                    SocketAddr::new(ip.into(), port)
                }
                AF_INET6 if length as usize >= size_of::<SOCKADDR_IN6>() => {
                    let ip = Ipv6Addr::from(
                        self.array::<16>(socket + offset_of!(SOCKADDR_IN6, sin6_addr))?,
                    );
                    let port = u16::from_be_bytes(
                        self.array(socket + offset_of!(SOCKADDR_IN6, sin6_port))?,
                    );
                    let flow = u32::from_be_bytes(
                        self.array(socket + offset_of!(SOCKADDR_IN6, sin6_flowinfo))?,
                    );
                    let scope = self.u32(socket + offset_of!(SOCKADDR_IN6, Anonymous))?;
                    SocketAddr::V6(SocketAddrV6::new(ip, port, flow, scope))
                }
                _ => return Err(invalid("unsupported or truncated adapter DNS socket")),
            };
            result.push(endpoint);
            pointer = self.pointer(offset + offset_of!(Server, Next))?;
        }
        Ok(result)
    }
}

fn parse(bytes: &[u8], base: usize) -> io::Result<Vec<DnsAdapter>> {
    let buffer = Buffer { bytes, base };
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    let mut pointer = base;
    while pointer != 0 {
        if !seen.insert(pointer) {
            return Err(invalid("cycle in adapter list"));
        }
        let offset = buffer.record(pointer, offset_of!(Adapter, Ipv6Metric) + size_of::<u32>())?;
        result.push(DnsAdapter {
            name: buffer.name(buffer.pointer(offset + offset_of!(Adapter, AdapterName))?)?,
            up: buffer.u32(offset + offset_of!(Adapter, OperStatus))? == 1,
            ipv4_metric: buffer.u32(offset + offset_of!(Adapter, Ipv4Metric))?,
            ipv6_metric: buffer.u32(offset + offset_of!(Adapter, Ipv6Metric))?,
            ipv6_if_index: buffer.u32(offset + offset_of!(Adapter, Ipv6IfIndex))?,
            dns_servers: buffer
                .servers(buffer.pointer(offset + offset_of!(Adapter, FirstDnsServerAddress))?)?,
        });
        pointer = buffer.pointer(offset + offset_of!(Adapter, Next))?;
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
