use std::collections::BTreeSet;
use std::fmt::Write as _;

use super::*;

const IPV4: &[(&str, bool)] = &[
    ("0.0.0.0", true),
    ("0.255.255.255", true),
    ("1.0.0.0", false),
    ("8.8.8.8", false),
    ("9.255.255.255", false),
    ("10.0.0.0", true),
    ("10.255.255.255", true),
    ("11.0.0.0", false),
    ("100.63.255.255", false),
    ("100.64.0.0", true),
    ("100.127.255.255", true),
    ("100.128.0.0", false),
    ("126.255.255.255", false),
    ("127.0.0.0", true),
    ("127.0.0.1", true),
    ("127.255.255.255", true),
    ("128.0.0.0", false),
    ("169.253.255.255", false),
    ("169.254.0.0", true),
    ("169.254.169.254", true),
    ("169.254.255.255", true),
    ("169.255.0.0", false),
    ("172.15.255.255", false),
    ("172.16.0.0", true),
    ("172.31.255.255", true),
    ("172.32.0.0", false),
    ("191.255.255.255", false),
    ("192.0.0.0", true),
    ("192.0.0.9", true),
    ("192.0.0.192", true),
    ("192.0.0.255", true),
    ("192.0.1.0", false),
    ("192.0.2.0", true),
    ("192.0.2.255", true),
    ("192.0.3.0", false),
    ("192.167.255.255", false),
    ("192.168.0.0", true),
    ("192.168.255.255", true),
    ("192.169.0.0", false),
    ("198.17.255.255", false),
    ("198.18.0.0", false),
    ("198.19.255.255", false),
    ("198.20.0.0", false),
    ("198.51.99.255", false),
    ("198.51.100.0", true),
    ("198.51.100.255", true),
    ("198.51.101.0", false),
    ("203.0.112.255", false),
    ("203.0.113.0", true),
    ("203.0.113.255", true),
    ("203.0.114.0", false),
    ("223.255.255.255", false),
    ("224.0.0.0", true),
    ("239.255.255.255", true),
    ("240.0.0.0", true),
    ("255.255.255.254", true),
    ("255.255.255.255", true),
];

const IPV6: &[(&str, bool)] = &[
    ("::", true),
    ("::1", true),
    ("2001:4860:4860::8888", false),
    ("2001:db7:ffff:ffff:ffff:ffff:ffff:ffff", false),
    ("2001:db8::", true),
    ("2001:db8:ffff:ffff:ffff:ffff:ffff:ffff", true),
    ("2001:db9::", false),
    ("3ffe:ffff:ffff:ffff:ffff:ffff:ffff:ffff", false),
    ("3fff::", true),
    ("3fff:fff:ffff:ffff:ffff:ffff:ffff:ffff", true),
    ("3fff:1000::", false),
    ("fbff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", false),
    ("fc00::", true),
    ("fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", true),
    ("fe00::", false),
    ("fe7f:ffff:ffff:ffff:ffff:ffff:ffff:ffff", false),
    ("fe80::", true),
    ("febf:ffff:ffff:ffff:ffff:ffff:ffff:ffff", true),
    ("fec0::", true),
    ("feff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", true),
    ("ff00::", true),
    ("ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", true),
    ("64:ff9b:1::", true),
    ("64:ff9b:1:ffff:ffff:ffff:ffff:ffff", true),
    ("64:ff9b:1:a9fe:a9:fe00:808:808", true),
    ("64:ff9b:0:1::", false),
    ("64:ff9b:2::", false),
];

fn ipv4_forms(ip: Ipv4Addr) -> BTreeSet<String> {
    let octets = ip.octets();
    let canonical = ip.to_string();
    let mut forms = BTreeSet::from([
        canonical.bytes().fold(String::new(), |mut text, byte| {
            write!(text, "%{byte:02X}").unwrap();
            text
        }),
        canonical.replace('.', "\u{ff0e}"),
        canonical.replace('.', "\u{3002}"),
    ]);
    for parts in 1..=4_u32 {
        let count = usize::try_from(parts).unwrap();
        let mut chunks: Vec<u32> = octets[..count - 1].iter().map(|n| u32::from(*n)).collect();
        chunks.push(
            octets[count - 1..]
                .iter()
                .fold(0, |n, byte| n * 256 + u32::from(*byte)),
        );
        for mut radices in 0..4_u32.pow(parts) {
            let formatted: Vec<_> = chunks
                .iter()
                .map(|chunk| {
                    let part = match radices % 4 {
                        0 => chunk.to_string(),
                        1 => format!("0{chunk:o}"),
                        2 => format!("0x{chunk:x}"),
                        _ => format!("0X{chunk:X}"),
                    };
                    radices /= 4;
                    part
                })
                .collect();
            let form = formatted.join(".");
            forms.insert(format!("{form}."));
            forms.insert(form);
        }
    }
    forms.insert(
        octets
            .iter()
            .map(|n| format!("0{n:03o}"))
            .collect::<Vec<_>>()
            .join("."),
    );
    forms
}

fn ipv6_forms(ip: Ipv6Addr) -> BTreeSet<String> {
    let segments = ip.segments();
    let words: Vec<_> = segments.iter().map(|word| format!("{word:x}")).collect();
    let mut forms = BTreeSet::from([
        ip.to_string(),
        words.join(":"),
        words.join(":").to_ascii_uppercase(),
    ]);
    let expanded = segments
        .iter()
        .map(|word| format!("{word:04x}"))
        .collect::<Vec<_>>()
        .join(":");
    forms.insert(expanded.to_ascii_uppercase());
    forms.insert(expanded);
    for start in 0..8 {
        for end in start + 1..=8 {
            if segments[start..end].iter().all(|word| *word == 0) {
                forms.insert(format!(
                    "{}::{}",
                    words[..start].join(":"),
                    words[end..].join(":")
                ));
            }
        }
    }
    forms.insert(
        segments
            .iter()
            .enumerate()
            .map(|(index, word)| {
                if index % 2 == 0 {
                    format!("{word:04X}")
                } else {
                    format!("{word:x}")
                }
            })
            .collect::<Vec<_>>()
            .join(":"),
    );
    let [.., a, b, c, d] = ip.octets();
    let dotted = format!("{a}.{b}.{c}.{d}");
    forms.insert(format!("{}:{dotted}", words[..6].join(":")));
    for start in 0..6 {
        for end in start + 1..=6 {
            if segments[start..end].iter().all(|word| *word == 0) {
                let suffix = words[end..6]
                    .iter()
                    .chain(std::iter::once(&dotted))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(":");
                forms.insert(format!("{}::{suffix}", words[..start].join(":")));
            }
        }
    }
    forms
}

fn check_form(text: &str, expected: IpAddr, private: bool, failures: &mut Vec<String>) {
    let authority = if expected.is_ipv6() {
        format!("[{text}]")
    } else {
        text.to_owned()
    };
    let url = Url::parse(&format!("http://{authority}/file")).unwrap();
    let parsed = match url.host().unwrap() {
        Host::Ipv4(ip) => IpAddr::V4(ip),
        Host::Ipv6(ip) => IpAddr::V6(ip),
        Host::Domain(_) => panic!("{text} parsed as a name instead of an address"),
    };
    assert_eq!(parsed, expected, "url parser: {text}");
    let standard = text.parse::<IpAddr>().ok();
    let standard_expected =
        (expected.is_ipv6() || text == expected.to_string()).then_some(expected);
    assert_eq!(standard, standard_expected, "std parser: {text}");
    if is_private(parsed) != private {
        failures.push(format!("{text} -> {parsed}: expected private={private}"));
    }
    let typed = Typed::default();
    if typed.check(&url).is_err() != private {
        failures.push(format!(
            "{text}: literal request policy disagrees with private={private}"
        ));
    }
    typed.add(&url);
    assert!(
        typed.check(&url).is_ok(),
        "a typed literal is trusted: {text}"
    );
}

#[test]
fn every_ipv4_radix_and_dotted_form_preserves_the_reserved_range() {
    let mut failures = Vec::new();
    for &(address, private) in IPV4 {
        let ip: Ipv4Addr = address.parse().unwrap();
        for form in ipv4_forms(ip) {
            check_form(&form, IpAddr::V4(ip), private, &mut failures);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_ipv6_compression_and_case_preserves_the_reserved_range() {
    let mut failures = Vec::new();
    for &(address, private) in IPV6 {
        let ip: Ipv6Addr = address.parse().unwrap();
        for form in ipv6_forms(ip) {
            check_form(&form, IpAddr::V6(ip), private, &mut failures);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn embedded_ipv4_ranges_are_checked_in_each_supported_ipv6_prefix() {
    let mut failures = Vec::new();
    for &(address, private) in IPV4 {
        let ip: Ipv4Addr = address.parse().unwrap();
        let [a, b, c, d] = ip.octets();
        let high = u16::from_be_bytes([a, b]);
        let low = u16::from_be_bytes([c, d]);
        for (ip, private) in [
            (ip.to_ipv6_mapped(), private),
            (Ipv6Addr::new(0, 0, 0, 0, 0, 0, high, low), private),
            (Ipv6Addr::new(0x2002, high, low, 0, 0, 0, 0, 1), private),
            (Ipv6Addr::new(0x64, 0xff9b, 0, 0, 0, 0, high, low), private),
            (Ipv6Addr::new(0x64, 0xff9b, 1, 0, 0, 0, high, low), true),
        ] {
            for form in ipv6_forms(ip) {
                check_form(&form, IpAddr::V6(ip), private, &mut failures);
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn scoped_and_malformed_literals_cannot_enter_the_url_address_path() {
    for text in [
        "fe80::1%lo",
        "fe80::1%25lo",
        "fe80::1%1",
        "::ffff:127.01.0.1",
        "1::2::3",
    ] {
        assert!(text.parse::<IpAddr>().is_err(), "std accepted {text}");
        assert!(
            Url::parse(&format!("http://[{text}]/")).is_err(),
            "url accepted {text}"
        );
    }
    for text in [
        "1.2.3.256",
        "4294967296",
        "0x100000000",
        "09.0.0.1",
        "1.2.3.4.5",
        "127..1",
    ] {
        assert!(text.parse::<IpAddr>().is_err(), "std accepted {text}");
        assert!(
            Url::parse(&format!("http://{text}/")).is_err(),
            "url accepted {text}"
        );
    }
    assert_eq!(
        Url::parse("http://010.0.0.1/").unwrap().host(),
        Some(Host::Ipv4(Ipv4Addr::new(8, 0, 0, 1)))
    );
}

#[test]
fn numeric_socket_scope_ids_are_not_accepted_as_url_hosts() {
    for (text, scope) in [
        ("[fe80::1%1]:443", 1),
        ("[fe80::1%01]:443", 1),
        ("[fe80::1%4294967295]:443", u32::MAX),
        ("[::1%0]:80", 0),
    ] {
        let address: std::net::SocketAddrV6 = text.parse().unwrap();
        assert_eq!(address.scope_id(), scope, "{text}");
        assert!(
            Url::parse(&format!("http://{text}/")).is_err(),
            "a scoped socket literal entered the URL parser: {text}"
        );
    }
    for text in ["[fe80::1%lo]:443", "[fe80::1%4294967296]:443"] {
        assert!(text.parse::<std::net::SocketAddrV6>().is_err(), "{text}");
        assert!(Url::parse(&format!("http://{text}/")).is_err(), "{text}");
    }
}

#[test]
fn unknown_host_errors_keep_the_host_and_explanation() {
    assert_eq!(
        Unknown("missing.invalid".to_owned()).to_string(),
        "missing.invalid is not a known host"
    );
}

#[tokio::test]
async fn a_name_rejected_as_nonexistent_keeps_its_permanent_error_category() {
    use reqwest::dns::Resolve as _;

    let resolver = Resolver::new(Typed::default(), false, Reach::Public);
    // an overlong label fails locally without querying a DNS server
    let long = "x".repeat(256);
    let Err(error) = resolver.resolve(long.parse().unwrap()).await else {
        panic!("an overlong name cannot resolve");
    };
    assert!(error.downcast_ref::<Unknown>().is_some(), "{error}");
}

#[tokio::test]
async fn an_invalid_lookup_input_is_not_mislabeled_as_a_nonexistent_host() {
    use reqwest::dns::Resolve as _;

    let resolver = Resolver::new(Typed::default(), false, Reach::Public);
    let Err(error) = resolver.resolve("bad\0host".parse().unwrap()).await else {
        panic!("a name containing NUL cannot resolve");
    };
    assert!(error.downcast_ref::<Unknown>().is_none());
    assert!(error.downcast_ref::<std::io::Error>().is_some());
}

#[tokio::test]
async fn public_numeric_proxy_targets_are_not_refused() {
    for address in [
        "8.8.8.8",
        "198.18.0.1",
        "::ffff:8.8.8.8",
        "64:ff9b::8.8.8.8",
    ] {
        assert!(private_address(address, 443).await.is_none(), "{address}");
    }
}

#[tokio::test]
async fn private_numeric_proxy_targets_name_the_refused_address() {
    for address in ["127.0.0.1", "169.254.169.254", "::1"] {
        let reason = private_address(address, 443).await.unwrap();
        assert!(reason.contains(address), "{reason}");
    }
}
