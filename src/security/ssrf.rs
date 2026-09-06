//! SSRF guardrails: target URL validation, private/reserved address blocking, a DNS resolver
//! that filters answers at resolution time (defeats DNS rebinding), and a redirect policy that
//! applies the same rules to every hop.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use url::{Host, Url};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

pub const MAX_URL_LEN: usize = 2048;
pub const MAX_REDIRECTS: usize = 5;

pub fn is_ip_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_v4_blocked(v4),
        IpAddr::V6(v6) => is_v6_blocked(v6),
    }
}

fn is_v4_blocked(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local() // includes the 169.254.169.254 cloud metadata endpoint
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0 // 0.0.0.0/8 "this network"
        || (a == 100 && (64..=127).contains(&b)) // 100.64.0.0/10 shared address space (CGNAT)
        || (a == 192 && b == 0 && c == 0) // 192.0.0.0/24 IETF protocol assignments
        || (a == 198 && (b == 18 || b == 19)) // 198.18.0.0/15 benchmarking
        || a >= 240 // 240.0.0.0/4 reserved (and 255.255.255.255)
}

fn is_v6_blocked(ip: Ipv6Addr) -> bool {
    let seg = ip.segments();
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() {
        return true;
    }
    if (seg[0] & 0xfe00) == 0xfc00 // fc00::/7 unique local
        || (seg[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
        || (seg[0] & 0xffc0) == 0xfec0 // fec0::/10 site-local (deprecated)
        || (seg[0] == 0x2001 && seg[1] == 0x0db8) // 2001:db8::/32 documentation
        || (seg[0] == 0x0100 && seg[1] == 0 && seg[2] == 0 && seg[3] == 0)
    // 100::/64 discard-only
    {
        return true;
    }
    // IPv4-mapped (::ffff:a.b.c.d) and IPv4-compatible (::a.b.c.d) addresses.
    if let Some(v4) = ip.to_ipv4() {
        if is_v4_blocked(v4) {
            return true;
        }
    }
    // 6to4 (2002:AABB:CCDD::/48) embeds the IPv4 address in segments 1-2.
    if seg[0] == 0x2002 {
        let v4 = Ipv4Addr::new(
            (seg[1] >> 8) as u8,
            seg[1] as u8,
            (seg[2] >> 8) as u8,
            seg[2] as u8,
        );
        if is_v4_blocked(v4) {
            return true;
        }
    }
    // Teredo (2001::/32) embeds the obfuscated client IPv4 in the last two segments.
    if seg[0] == 0x2001 && seg[1] == 0 {
        let c = !seg[6];
        let d = !seg[7];
        let v4 = Ipv4Addr::new((c >> 8) as u8, c as u8, (d >> 8) as u8, d as u8);
        if is_v4_blocked(v4) {
            return true;
        }
    }
    false
}

pub fn is_blocked_hostname(host: &str) -> bool {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    h == "localhost"
        || h.ends_with(".localhost")
        || h.ends_with(".local")
        || h.ends_with(".internal")
        || h.ends_with(".localdomain")
}

/// Validate a host (domain or IP literal) against the block lists.
pub fn check_host(host: &Host<&str>) -> Result<(), String> {
    match host {
        Host::Domain(d) => {
            if is_blocked_hostname(d) {
                Err(format!("Target host '{d}' is not allowed"))
            } else {
                Ok(())
            }
        }
        Host::Ipv4(ip) => {
            if is_v4_blocked(*ip) {
                Err(format!(
                    "Target address {ip} is private or reserved and cannot be monitored"
                ))
            } else {
                Ok(())
            }
        }
        Host::Ipv6(ip) => {
            if is_v6_blocked(*ip) {
                Err(format!(
                    "Target address {ip} is private or reserved and cannot be monitored"
                ))
            } else {
                Ok(())
            }
        }
    }
}

/// Parse and validate a monitor target URL (scheme, host, block lists).
pub fn validate_target_url(raw: &str, allow_private: bool) -> Result<Url, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("URL is required".to_string());
    }
    if raw.len() > MAX_URL_LEN {
        return Err(format!("URL is too long (max {MAX_URL_LEN} characters)"));
    }
    let url = Url::parse(raw).map_err(|e| format!("Invalid URL: {e}"))?;
    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(format!(
                "Unsupported URL scheme '{other}': only http and https can be monitored"
            ))
        }
    }
    let host = url
        .host()
        .ok_or_else(|| "URL must include a host name".to_string())?;
    if !allow_private {
        check_host(&host)?;
    }
    Ok(url)
}

/// Creation-time DNS check: reject domains that resolve to blocked addresses. DNS failures are
/// tolerated (the monitor will simply record failing probes).
pub async fn check_resolves_public(url: &Url) -> Result<(), String> {
    let Some(Host::Domain(domain)) = url.host() else {
        return Ok(());
    };
    let port = url.port_or_known_default().unwrap_or(80);
    match tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::lookup_host((domain, port)),
    )
    .await
    {
        Ok(Ok(addrs)) => {
            let addrs: Vec<SocketAddr> = addrs.collect();
            if addrs.iter().any(|a| is_ip_blocked(a.ip())) {
                return Err(format!(
                    "Target '{domain}' resolves to a private or reserved address, which cannot be monitored"
                ));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// DNS resolver used by the probe HTTP clients. Addresses in blocked ranges are removed from
/// every answer, so a hostname can never be used to reach internal infrastructure, even if its
/// records change after the monitor was created.
#[derive(Debug, Clone)]
pub struct SafeResolver {
    pub allow_private: bool,
}

#[derive(Debug)]
struct BlockedAddress(String);

impl std::fmt::Display for BlockedAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "host '{}' resolves only to private or reserved addresses (blocked)",
            self.0
        )
    }
}

impl std::error::Error for BlockedAddress {}

async fn resolve_filtered(host: String, allow_private: bool) -> Result<Addrs, BoxError> {
    let all: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
    let allowed: Vec<SocketAddr> = all
        .iter()
        .copied()
        .filter(|a| allow_private || !is_ip_blocked(a.ip()))
        .collect();
    if allowed.is_empty() {
        if all.is_empty() {
            let err = std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no addresses found for {host}"),
            );
            return Err(Box::new(err));
        }
        return Err(Box::new(BlockedAddress(host)));
    }
    Ok(Box::new(allowed.into_iter()))
}

impl Resolve for SafeResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(resolve_filtered(
            name.as_str().to_string(),
            self.allow_private,
        ))
    }
}

/// Redirect policy: at most `MAX_REDIRECTS` hops, http/https only, every hop checked against
/// the block lists (hostnames are additionally filtered by `SafeResolver` when resolved).
pub fn redirect_policy(allow_private: bool) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error(format!("too many redirects (max {MAX_REDIRECTS})"));
        }
        let url = attempt.url();
        if !matches!(url.scheme(), "http" | "https") {
            return attempt.error("redirect to unsupported scheme");
        }
        if !allow_private {
            match url.host() {
                None => return attempt.error("redirect target has no host"),
                Some(host) => {
                    if let Err(msg) = check_host(&host) {
                        return attempt.error(msg);
                    }
                }
            }
        }
        attempt.follow()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_private_and_reserved_v4() {
        for ip in [
            "127.0.0.1",
            "127.255.255.254",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "198.18.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "192.0.2.1",
        ] {
            assert!(is_ip_blocked(ip.parse().unwrap()), "{ip} should be blocked");
        }
        for ip in [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.216.34",
            "172.32.0.1",
            "100.128.0.1",
        ] {
            assert!(
                !is_ip_blocked(ip.parse().unwrap()),
                "{ip} should be allowed"
            );
        }
    }

    #[test]
    fn blocks_private_and_mapped_v6() {
        for ip in [
            "::1",
            "::",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:10.1.2.3",
            "2001:db8::1",
            "ff02::1",
            "2002:7f00:0001::1",
        ] {
            assert!(is_ip_blocked(ip.parse().unwrap()), "{ip} should be blocked");
        }
        assert!(!is_ip_blocked("2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn validates_urls() {
        assert!(validate_target_url("https://example.com/health", false).is_ok());
        assert!(validate_target_url("http://93.184.216.34/", false).is_ok());
        assert!(validate_target_url("ftp://example.com", false).is_err());
        assert!(validate_target_url("not a url", false).is_err());
        assert!(validate_target_url("http://localhost:8080/", false).is_err());
        assert!(validate_target_url("http://127.0.0.1/", false).is_err());
        assert!(validate_target_url("http://[::1]/", false).is_err());
        assert!(validate_target_url("http://169.254.169.254/latest/meta-data", false).is_err());
        assert!(validate_target_url("http://router.local/", false).is_err());
        // Escape hatch for self-hosted LAN monitoring.
        assert!(validate_target_url("http://127.0.0.1/", true).is_ok());
        assert!(
            validate_target_url(&format!("https://example.com/{}", "a".repeat(3000)), false)
                .is_err()
        );
    }
}
