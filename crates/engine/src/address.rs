//! The addresses a run refuses to connect to, whoever named them.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::error::{Error, ErrorKind};

#[must_use]
pub fn refused(address: IpAddr) -> Option<&'static str> {
    match address {
        IpAddr::V4(four) => refused_v4(four),
        IpAddr::V6(six) => refused_v6(six),
    }
}

fn refused_v4(address: Ipv4Addr) -> Option<&'static str> {
    let [first, second, ..] = address.octets();
    if address.is_unspecified() || first == 0 {
        return Some("this host");
    }
    if address.is_loopback() {
        return Some("the loopback interface");
    }
    if address.is_link_local() {
        return Some("the link-local range, which holds the cloud metadata endpoint");
    }
    if address.is_private() {
        return Some("a private range");
    }
    if first == 100 && (64..128).contains(&second) {
        return Some("the carrier-grade range");
    }
    if first == 192 && second == 0 && address.octets()[2] == 0 {
        return Some("the protocol assignment range");
    }
    if address.is_documentation() {
        return Some("a documentation range");
    }
    if first == 198 && (18..20).contains(&second) {
        return Some("the benchmarking range");
    }
    if address.is_multicast() {
        return Some("a multicast range");
    }
    if address.is_broadcast() || first >= 240 {
        return Some("a reserved range");
    }
    None
}

fn refused_v6(address: Ipv6Addr) -> Option<&'static str> {
    if let Some(four) = mapped_v4(address) {
        return refused_v4(four);
    }
    let first = address.segments()[0];
    if address.is_unspecified() {
        return Some("this host");
    }
    if address.is_loopback() {
        return Some("the loopback interface");
    }
    if first & 0xfe00 == 0xfc00 {
        return Some("a unique-local range");
    }
    if first & 0xffc0 == 0xfe80 {
        return Some("the link-local range");
    }
    if address.is_multicast() {
        return Some("a multicast range");
    }
    if address.segments()[..2] == [0x2001, 0x0db8] {
        return Some("a documentation range");
    }
    None
}

fn mapped_v4(address: Ipv6Addr) -> Option<Ipv4Addr> {
    let segments = address.segments();
    let embedded = |high: u16, low: u16| {
        Ipv4Addr::new(
            (high >> 8) as u8,
            (high & 0xff) as u8,
            (low >> 8) as u8,
            (low & 0xff) as u8,
        )
    };
    if segments[..5] == [0, 0, 0, 0, 0] && segments[5] == 0xffff {
        return Some(embedded(segments[6], segments[7]));
    }
    if segments[..4] == [0x0064, 0xff9b, 0, 0] && segments[4] == 0 && segments[5] == 0 {
        return Some(embedded(segments[6], segments[7]));
    }
    if segments[..6] == [0, 0, 0, 0, 0, 0] && (segments[6] != 0 || segments[7] > 1) {
        return Some(embedded(segments[6], segments[7]));
    }
    None
}

/// # Errors
/// `policy.address_refused` naming the host, the address it answered with, and
/// the class that address falls in.
pub fn allowed(host: &str, address: IpAddr) -> Result<(), Error> {
    match refused(address) {
        None => Ok(()),
        Some(class) => Err(Error::new(
            ErrorKind::PolicyAddressRefused,
            format!(
                "ask for a location that is served publicly, because {host} resolved to {address}, which is {class}, and a run never opens a connection inside the machine or the network it is running in"
            ),
        )),
    }
}
