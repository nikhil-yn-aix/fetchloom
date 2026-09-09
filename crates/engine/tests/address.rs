//! The addresses a run refuses to open a connection to.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a value that did not parse is the assertion"
)]

use blake3 as _;
use rayon as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use toml as _;

use std::net::IpAddr;

use fetchloom_engine::address;
use fetchloom_engine::error::ErrorKind;

#[test]
fn the_cloud_metadata_endpoint_is_refused() {
    let metadata: IpAddr = "169.254.169.254".parse().unwrap();
    let refused = address::allowed("metadata.example", metadata).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::PolicyAddressRefused);
    assert!(
        refused.next_action().contains("169.254.169.254"),
        "the refusal does not name the address: {}",
        refused.next_action()
    );
    assert!(
        refused.next_action().contains("metadata.example"),
        "the refusal does not name the host: {}",
        refused.next_action()
    );
}

#[test]
fn every_address_inside_the_machine_or_its_network_is_refused() {
    for stated in [
        "127.0.0.1",
        "0.0.0.0",
        "10.1.2.3",
        "172.16.5.4",
        "192.168.1.1",
        "169.254.1.1",
        "100.64.0.1",
        "198.18.0.1",
        "192.0.0.1",
        "192.0.2.1",
        "224.0.0.1",
        "255.255.255.255",
        "240.0.0.1",
        "::1",
        "::",
        "fc00::1",
        "fd00::1",
        "fe80::1",
        "ff02::1",
        "2001:db8::1",
        "::ffff:169.254.169.254",
        "64:ff9b::169.254.169.254",
    ] {
        let address: IpAddr = stated.parse().unwrap();
        assert!(
            address::refused(address).is_some(),
            "{stated} was allowed through"
        );
    }
}

#[test]
fn a_public_address_is_allowed() {
    for stated in [
        "93.184.216.34",
        "8.8.8.8",
        "1.1.1.1",
        "2606:2800:220:1:248:1893:25c8:1946",
    ] {
        let address: IpAddr = stated.parse().unwrap();
        assert!(
            address::refused(address).is_none(),
            "{stated} was refused, and it is served publicly"
        );
    }
}
