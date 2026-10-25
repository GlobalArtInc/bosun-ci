use std::net::{IpAddr, SocketAddr};

use axum::http::HeaderMap;
use ipnet::IpNet;

const FORWARDED_FOR: &str = "x-forwarded-for";

pub(crate) fn client(peer: IpAddr, headers: &HeaderMap, trusted: &[IpNet]) -> IpAddr {
    let hops: Vec<&str> = headers
        .get_all(FORWARDED_FOR)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .collect();
    let mut address = peer.to_canonical();
    for hop in hops.into_iter().rev() {
        if !trusted.iter().any(|network| network.contains(&address)) {
            break;
        }
        match parse(hop) {
            Some(next) => address = next,
            None => break,
        }
    }
    address
}

pub(crate) fn network(text: &str) -> Option<IpNet> {
    text.parse::<IpNet>()
        .ok()
        .or_else(|| text.parse::<IpAddr>().ok().map(IpNet::from))
}

fn parse(hop: &str) -> Option<IpAddr> {
    hop.parse::<IpAddr>()
        .or_else(|_| hop.parse::<SocketAddr>().map(|socket| socket.ip()))
        .ok()
        .map(|address| address.to_canonical())
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use axum::http::{HeaderMap, HeaderValue};

    use super::{client, network};

    fn address(text: &str) -> IpAddr {
        text.parse().expect("an address")
    }

    fn forwarded(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(
                "x-forwarded-for",
                HeaderValue::from_str(value).expect("a header value"),
            );
        }
        headers
    }

    #[test]
    fn the_forwarded_address_counts_only_behind_a_trusted_proxy() {
        let trusted = [
            network("10.0.0.0/8").expect("a network"),
            network("192.0.2.7").expect("an address"),
        ];
        for (peer, values, expected) in [
            ("203.0.113.9", &["198.51.100.1"][..], "203.0.113.9"),
            ("10.1.2.3", &[][..], "10.1.2.3"),
            ("10.1.2.3", &["198.51.100.1"][..], "198.51.100.1"),
            ("192.0.2.7", &["198.51.100.1"][..], "198.51.100.1"),
            ("192.0.2.8", &["198.51.100.1"][..], "192.0.2.8"),
            (
                "10.1.2.3",
                &["198.51.100.66, 198.51.100.1, 10.9.9.9"][..],
                "198.51.100.1",
            ),
            (
                "10.1.2.3",
                &["198.51.100.66", "198.51.100.1", "10.9.9.9"][..],
                "198.51.100.1",
            ),
            ("10.1.2.3", &["10.0.0.1, 10.0.0.2"][..], "10.0.0.1"),
            ("10.1.2.3", &["198.51.100.1:4711"][..], "198.51.100.1"),
            ("10.1.2.3", &["[2001:db8::1]:4711"][..], "2001:db8::1"),
            ("10.1.2.3", &["unknown, 10.0.0.5"][..], "10.0.0.5"),
            ("10.1.2.3", &["198.51.100.1, garbage"][..], "10.1.2.3"),
            (
                "::ffff:10.1.2.3",
                &["::ffff:198.51.100.1"][..],
                "198.51.100.1",
            ),
        ] {
            assert_eq!(
                client(address(peer), &forwarded(values), &trusted),
                address(expected),
                "{peer} with {values:?}"
            );
        }
        assert_eq!(
            client(address("10.1.2.3"), &forwarded(&["198.51.100.1"]), &[]),
            address("10.1.2.3")
        );
    }

    #[test]
    fn a_trusted_proxy_is_an_address_or_a_network() {
        for text in [
            "10.0.0.0/8",
            "127.0.0.1",
            "::1",
            "2001:db8::/32",
            "10.1.2.3/32",
        ] {
            assert!(network(text).is_some(), "{text}");
        }
        for text in ["", "10.0.0.0/33", "localhost", "10.0.0", "10.0.0.0/8/8"] {
            assert!(network(text).is_none(), "{text}");
        }
    }
}
