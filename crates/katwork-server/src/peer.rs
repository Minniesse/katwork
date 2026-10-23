use axum::http::HeaderMap;
use std::net::IpAddr;

// Group IPv6 privacy addresses by /64; mapped IPv4 shares its native IPv4 bucket.
pub(crate) fn ip_bucket(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(ip) => IpAddr::V4(ip),
            None => IpAddr::V6((u128::from(ip) & (u128::MAX << 64)).into()),
        },
        ip => ip,
    }
}

pub(crate) fn client_ip(
    peer: IpAddr,
    headers: &HeaderMap,
    config: &katwork::config::Config,
) -> IpAddr {
    if !config.trusts_proxy(peer) {
        return peer;
    }
    let mut values = headers.get_all("x-forwarded-for").iter();
    let Some(value) = values.next().and_then(|value| value.to_str().ok()) else {
        return peer;
    };
    if values.next().is_some() {
        return peer;
    }
    let mut client = peer;
    // Walk from the trusted peer; ignore any attacker-supplied prefix beyond the first untrusted hop.
    for hop in value.rsplit(',') {
        if !config.trusts_proxy(client) {
            break;
        }
        let Ok(ip) = hop.trim().parse() else {
            return peer;
        };
        client = ip;
    }
    client
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_buckets_share_ipv6_prefixes_and_mapped_ipv4() {
        for (left, right) in [
            ("2001:db8:1:2::1", "2001:db8:1:2:ffff:ffff:ffff:ffff"),
            ("192.0.2.1", "::ffff:192.0.2.1"),
        ] {
            assert_eq!(
                ip_bucket(left.parse().unwrap()),
                ip_bucket(right.parse().unwrap())
            );
        }
        assert_ne!(
            ip_bucket("2001:db8:1:2::1".parse().unwrap()),
            ip_bucket("2001:db8:1:3::1".parse().unwrap())
        );
        assert_ne!(
            ip_bucket("192.0.2.1".parse().unwrap()),
            ip_bucket("192.0.2.2".parse().unwrap())
        );
    }
}
