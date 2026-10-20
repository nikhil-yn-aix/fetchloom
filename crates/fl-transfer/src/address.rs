//! The address policy: which addresses a redirect may lead to.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// IPv4 ranges that are never contacted after a redirect: this network, private, shared
/// (CGNAT), loopback, link local, IETF protocol assignments, documentation, the old 6to4
/// relay, benchmarking, multicast, reserved and broadcast.
const REFUSED_V4: [(u32, u8); 15] = [
    (0x0000_0000, 8),
    (0x0a00_0000, 8),
    (0x6440_0000, 10),
    (0x7f00_0000, 8),
    (0xa9fe_0000, 16),
    (0xac10_0000, 12),
    (0xc000_0000, 24),
    (0xc000_0200, 24),
    (0xc058_6300, 24),
    (0xc0a8_0000, 16),
    (0xc612_0000, 15),
    (0xc633_6400, 24),
    (0xcb00_7100, 24),
    (0xe000_0000, 4),
    (0xf000_0000, 4),
];

/// IPv6 ranges that are never contacted after a redirect: unspecified, loopback, the
/// deprecated IPv4 compatible form, local use NAT64, discard, IETF protocol assignments with
/// Teredo, documentation, segment routing, unique local, link local, the deprecated site local
/// and multicast.
const REFUSED_V6: [(u128, u8); 13] = [
    (0, 128),
    (1, 128),
    (0, 96),
    (0x0064_ff9b_0001_u128 << 80, 48),
    (0x0100_u128 << 112, 64),
    (0x2001_u128 << 112, 23),
    (0x2001_0db8_u128 << 96, 32),
    (0x3fff_u128 << 112, 20),
    (0x5f00_u128 << 112, 16),
    (0xfc00_u128 << 112, 7),
    (0xfe80_u128 << 112, 10),
    (0xfec0_u128 << 112, 10),
    (0xff00_u128 << 112, 8),
];

const NAT64: (u128, u8) = (0x0064_ff9b_u128 << 96, 96);
const SIX_TO_FOUR: (u128, u8) = (0x2002_u128 << 112, 16);

/// Whether `address` may be contacted when a redirect leads to it: true only for addresses
/// that are reachable on the public internet. An IPv4 address carried inside an IPv6 one, as
/// mapped, NAT64 or 6to4, is judged as itself.
#[must_use]
pub fn is_public(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(address: Ipv4Addr) -> bool {
    let bits = address.to_bits();
    !REFUSED_V4
        .iter()
        .any(|(network, prefix)| within(bits, *network, *prefix))
}

fn is_public_v6(address: Ipv6Addr) -> bool {
    if let Some(v4) = address.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let bits = address.to_bits();
    if within128(bits, NAT64.0, NAT64.1) {
        return is_public_v4(Ipv4Addr::from_bits(low32(bits)));
    }
    if within128(bits, SIX_TO_FOUR.0, SIX_TO_FOUR.1) {
        return is_public_v4(Ipv4Addr::from_bits(low32(bits >> 80)));
    }
    !REFUSED_V6
        .iter()
        .any(|(network, prefix)| within128(bits, *network, *prefix))
}

fn low32(bits: u128) -> u32 {
    u32::try_from(bits & u128::from(u32::MAX)).unwrap_or(u32::MAX)
}

fn within(bits: u32, network: u32, prefix: u8) -> bool {
    let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
    bits & mask == network
}

fn within128(bits: u128, network: u128, prefix: u8) -> bool {
    let mask = u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0);
    bits & mask == network
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public(text: &str) -> bool {
        is_public(text.parse().unwrap())
    }

    #[test]
    fn every_special_range_is_refused_at_both_ends() {
        for text in [
            "0.0.0.0",
            "0.255.255.255",
            "10.0.0.1",
            "10.255.255.255",
            "100.64.0.0",
            "100.127.255.255",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "192.88.99.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.7",
            "203.0.113.9",
            "224.0.0.1",
            "239.255.255.255",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::7f00:1",
            "64:ff9b:1::1",
            "100::1",
            "2001::1",
            "2001:1ff:ffff::1",
            "2001:db8::1",
            "3fff::1",
            "5f00::1",
            "fc00::1",
            "fdff::1",
            "fe80::1",
            "febf::1",
            "fec0::1",
            "ff02::1",
        ] {
            assert!(!public(text), "{text} was allowed");
        }
    }

    #[test]
    fn public_addresses_beside_the_ranges_are_allowed() {
        for text in [
            "1.1.1.1",
            "9.255.255.255",
            "11.0.0.0",
            "100.63.255.255",
            "100.128.0.0",
            "126.255.255.255",
            "169.253.255.255",
            "172.15.255.255",
            "172.32.0.0",
            "192.0.1.0",
            "192.167.255.255",
            "198.17.255.255",
            "198.20.0.0",
            "223.255.255.255",
            "2001:200::1",
            "2001:4860:4860::8888",
            "2606:4700::1111",
            "fbff::1",
        ] {
            assert!(public(text), "{text} was refused");
        }
    }

    #[test]
    fn ipv4_inside_ipv6_is_judged_as_itself() {
        for (text, expected) in [
            ("::ffff:169.254.169.254", false),
            ("::ffff:10.0.0.1", false),
            ("::ffff:8.8.8.8", true),
            ("64:ff9b::a9fe:a9fe", false),
            ("64:ff9b::808:808", true),
            ("2002:a9fe:a9fe::1", false),
            ("2002:7f00:1::", false),
            ("2002:808:808::1", true),
        ] {
            assert_eq!(public(text), expected, "{text}");
        }
    }
}
