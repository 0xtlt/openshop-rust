//! Who is calling: the address of the client of a request.

use std::net::{IpAddr, Ipv6Addr};

use axum::extract::{ConnectInfo, Request};
use axum::http::HeaderMap;

use super::linger::Peer;

/// The address of the client of a request, when it is known.
///
/// It is the address of the peer of the connection. That is the address of a
/// reverse proxy when the application is behind one, and every client then
/// looks the same. So when the peer is on a private network, a loopback or a
/// link-local address (where a proxy of the application lives), the last
/// address of `X-Forwarded-For` is taken: it is the one that proxy added for
/// its own peer, where the others are written by the client. A peer on a public
/// address is never believed about the headers it sends.
pub(crate) fn client_ip(peer: Option<&ConnectInfo<Peer>>, headers: &HeaderMap) -> Option<IpAddr> {
    let ConnectInfo(Peer(peer)) = peer?;
    let peer = peer.ip().to_canonical();
    if !is_internal(peer) {
        return Some(peer);
    }
    let forwarded = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .next_back()
        .and_then(|address| address.trim().parse::<IpAddr>().ok())
        .map(|address| address.to_canonical());
    Some(forwarded.unwrap_or(peer))
}

/// [`client_ip`] of a request.
pub(crate) fn request_client_ip(request: &Request) -> Option<IpAddr> {
    client_ip(request.extensions().get::<ConnectInfo<Peer>>(), request.headers())
}

fn is_internal(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            address.is_loopback() || address.is_private() || address.is_link_local() || address.is_unspecified()
        }
        IpAddr::V6(address) => {
            address.is_loopback()
                || address.is_unspecified()
                // Unique local (fc00::/7) and link-local (fe80::/10).
                || (address.segments()[0] & 0xfe00) == 0xfc00
                || (address.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// What a client is counted under: its address, a whole `/64` for IPv6, where
/// a single host owns billions of addresses.
pub(crate) fn client_key(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V4(_) => address,
        IpAddr::V6(address) => {
            let [a, b, c, d, ..] = address.segments();
            IpAddr::V6(Ipv6Addr::new(a, b, c, d, 0, 0, 0, 0))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use axum::http::HeaderValue;

    use super::*;

    fn peer(address: &str) -> Option<ConnectInfo<Peer>> {
        Some(ConnectInfo(Peer(SocketAddr::new(address.parse().unwrap(), 4000))))
    }

    fn forwarded(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append("x-forwarded-for", HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    fn ip(address: &str) -> Option<IpAddr> {
        Some(address.parse().unwrap())
    }

    #[test]
    fn a_public_peer_is_never_believed_about_its_headers() {
        let headers = forwarded(&["10.9.9.9"]);
        assert_eq!(client_ip(peer("203.0.113.7").as_ref(), &headers), ip("203.0.113.7"));
        assert_eq!(client_ip(peer("2001:db8::1").as_ref(), &headers), ip("2001:db8::1"));
    }

    #[test]
    fn a_proxy_on_a_private_network_names_the_client_it_saw() {
        for proxy in ["127.0.0.1", "::1", "10.0.0.2", "172.16.5.4", "192.168.1.1", "169.254.1.1", "fd00::2", "::ffff:10.0.0.2"] {
            // The client wrote the first address; the proxy added the last.
            let headers = forwarded(&["6.6.6.6, 198.51.100.4"]);
            assert_eq!(client_ip(peer(proxy).as_ref(), &headers), ip("198.51.100.4"), "{proxy}");
            let headers = forwarded(&["6.6.6.6", "198.51.100.4"]);
            assert_eq!(client_ip(peer(proxy).as_ref(), &headers), ip("198.51.100.4"), "{proxy}");
        }
        // Nothing usable: the proxy itself is the client.
        for headers in [HeaderMap::new(), forwarded(&["unknown"]), forwarded(&["198.51.100.4, nope"])] {
            assert_eq!(client_ip(peer("10.0.0.2").as_ref(), &headers), ip("10.0.0.2"));
        }
    }

    #[test]
    fn without_a_peer_the_client_is_unknown() {
        assert_eq!(client_ip(None, &forwarded(&["198.51.100.4"])), None);
    }

    #[test]
    fn ipv6_clients_are_counted_by_network() {
        let key = |address: &str| client_key(address.parse().unwrap());
        assert_eq!(key("2001:db8:1:2:aaaa::1"), key("2001:db8:1:2:bbbb::2"));
        assert_ne!(key("2001:db8:1:2::1"), key("2001:db8:1:3::1"));
        assert_eq!(key("198.51.100.4"), "198.51.100.4".parse::<IpAddr>().unwrap());
    }
}
