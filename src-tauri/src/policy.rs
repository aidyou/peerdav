//! Share policy evaluation.
//!
//! Semantics fixed by the product requirements:
//! - Any matching deny rule wins.
//! - Otherwise at least one allow rule must match; an empty allow list denies.
//! - A MAC rule can only match when the peer MAC is known (ARP lookup). When
//!   the MAC is unknown the MAC rule does not match, but independent IP and
//!   network rules are still evaluated.

use std::fmt::Write as _;
use std::net::IpAddr;

use ipnet::IpNet;

use crate::error::DomainError;
use crate::types::{AccessRule, RuleKind, RuleMatcher, SharePermissions};

/// Peer identity available to the policy engine.
#[derive(Debug, Clone, Copy)]
pub struct PeerContext {
    pub ip: IpAddr,
    /// `None` when the neighbor lookup is unavailable or incomplete.
    pub mac: Option<[u8; 6]>,
}

/// Result of evaluating a share rule set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyDecision {
    Allow,
    Deny,
}

/// A parsed matcher. Values are canonicalized before they reach storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompiledMatcher {
    Ip(IpAddr),
    Network(IpNet),
    Mac([u8; 6]),
}

impl CompiledMatcher {
    fn matches(&self, peer: &PeerContext) -> bool {
        match self {
            Self::Ip(address) => canonical_ip(*address) == canonical_ip(peer.ip),
            Self::Network(network) => network.contains(&canonical_ip(peer.ip)),
            Self::Mac(address) => peer.mac.is_some_and(|mac| mac == *address),
        }
    }

    fn normalized(&self) -> RuleMatcher {
        match self {
            Self::Ip(address) => RuleMatcher::Ip(canonical_ip(*address).to_string()),
            Self::Network(network) => RuleMatcher::Network(network.trunc().to_string()),
            Self::Mac(address) => RuleMatcher::Mac(format_mac(address)),
        }
    }
}

/// One parsed rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledRule {
    pub kind: RuleKind,
    pub matcher: CompiledMatcher,
}

/// Permissions a request method needs. `MOVE`/`COPY` require a conservative
/// union because the gate does not inspect the destination share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RequiredPermissions {
    pub read: bool,
    pub write: bool,
    pub delete: bool,
}

impl RequiredPermissions {
    pub fn is_met_by(&self, permissions: SharePermissions) -> bool {
        (!self.read || permissions.can_read)
            && (!self.write || permissions.can_write)
            && (!self.delete || permissions.can_delete)
    }
}

/// Parses one rule, validating and canonicalizing its matcher.
pub fn compile_rule(rule: &AccessRule) -> Result<CompiledRule, DomainError> {
    let matcher = match &rule.matcher {
        RuleMatcher::Ip(value) => {
            let address = value
                .trim()
                .parse::<IpAddr>()
                .map_err(|error| invalid_rule(format!("invalid IP address `{value}`: {error}")))?;
            CompiledMatcher::Ip(address)
        }
        RuleMatcher::Network(value) => {
            let network = value
                .trim()
                .parse::<IpNet>()
                .map_err(|error| invalid_rule(format!("invalid network `{value}`: {error}")))?;
            CompiledMatcher::Network(network)
        }
        RuleMatcher::Mac(value) => CompiledMatcher::Mac(parse_mac(value)?),
    };

    Ok(CompiledRule {
        kind: rule.kind,
        matcher,
    })
}

/// Parses, canonicalizes, and de-duplicates a rule set.
pub fn normalize_rules(rules: &[AccessRule]) -> Result<Vec<AccessRule>, DomainError> {
    let mut normalized: Vec<AccessRule> = Vec::with_capacity(rules.len());
    for rule in rules {
        let compiled = compile_rule(rule)?;
        let candidate = AccessRule {
            kind: compiled.kind,
            matcher: compiled.matcher.normalized(),
        };
        if !normalized.contains(&candidate) {
            normalized.push(candidate);
        }
    }
    Ok(normalized)
}

/// Evaluates already-compiled rules. Deny priority and the empty-allow rule
/// are both enforced here.
pub fn evaluate(compiled: &[CompiledRule], peer: &PeerContext) -> PolicyDecision {
    let mut allowed = false;
    for rule in compiled {
        if !rule.matcher.matches(peer) {
            continue;
        }
        match rule.kind {
            RuleKind::Deny => return PolicyDecision::Deny,
            RuleKind::Allow => allowed = true,
        }
    }

    if allowed {
        PolicyDecision::Allow
    } else {
        PolicyDecision::Deny
    }
}

/// Convenience wrapper for callers that only have wire-format rules.
pub fn evaluate_rules(
    rules: &[AccessRule],
    peer: &PeerContext,
) -> Result<PolicyDecision, DomainError> {
    let compiled = rules
        .iter()
        .map(compile_rule)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(evaluate(&compiled, peer))
}

/// Maps an HTTP method to the permissions it requires. `None` means the
/// gateway must reject the method (unknown methods, and PROPPATCH which the
/// server intentionally does not support).
pub fn required_for_method(method: &http::Method) -> Option<RequiredPermissions> {
    let read = RequiredPermissions {
        read: true,
        ..RequiredPermissions::default()
    };
    let write = RequiredPermissions {
        write: true,
        ..RequiredPermissions::default()
    };
    let delete = RequiredPermissions {
        delete: true,
        ..RequiredPermissions::default()
    };

    match method.as_str() {
        "OPTIONS" => Some(RequiredPermissions::default()),
        "GET" | "HEAD" | "PROPFIND" | "LOCK" | "UNLOCK" => Some(read),
        "PUT" | "MKCOL" | "PATCH" => Some(write),
        "DELETE" => Some(delete),
        "COPY" => Some(RequiredPermissions {
            read: true,
            write: true,
            delete: false,
        }),
        "MOVE" => Some(RequiredPermissions {
            read: false,
            write: true,
            delete: true,
        }),
        _ => None,
    }
}

/// Parses a MAC address in `aa:bb:cc:dd:ee:ff`, `aa-bb-...`, or bare
/// 12-hex-digit form.
pub fn parse_mac(value: &str) -> Result<[u8; 6], DomainError> {
    let trimmed = value.trim();
    let mut digits = String::with_capacity(12);
    for (index, character) in trimmed.chars().enumerate() {
        match character {
            ':' | '-' => {
                if index == 0 || index + 1 == trimmed.len() {
                    return Err(invalid_rule(format!("invalid MAC address `{value}`")));
                }
            }
            character if character.is_ascii_hexdigit() => digits.push(character),
            _ => return Err(invalid_rule(format!("invalid MAC address `{value}`"))),
        }
    }
    if digits.len() != 12 {
        return Err(invalid_rule(format!("invalid MAC address `{value}`")));
    }

    let mut mac = [0_u8; 6];
    for (index, byte) in mac.iter_mut().enumerate() {
        let start = index * 2;
        *byte = u8::from_str_radix(&digits[start..start + 2], 16)
            .map_err(|error| invalid_rule(format!("invalid MAC address `{value}`: {error}")))?;
    }
    Ok(mac)
}

pub fn format_mac(mac: &[u8; 6]) -> String {
    let mut rendered = String::with_capacity(17);
    for (index, byte) in mac.iter().enumerate() {
        if index > 0 {
            rendered.push(':');
        }
        let _ = write!(rendered, "{byte:02x}");
    }
    rendered
}

/// Collapses IPv4-mapped IPv6 addresses so a peer reported as
/// `::ffff:192.168.1.5` matches ordinary IPv4 rules.
pub fn canonical_ip(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(address) => match address.to_ipv4_mapped() {
            Some(address) => IpAddr::V4(address),
            None => IpAddr::V6(address),
        },
        IpAddr::V4(_) => address,
    }
}

fn invalid_rule(detail: String) -> DomainError {
    DomainError::InvalidPolicyRule { detail }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn peer(ip: &str, mac: Option<&str>) -> PeerContext {
        PeerContext {
            ip: ip.parse().expect("test ip"),
            mac: mac.map(|value| parse_mac(value).expect("test mac")),
        }
    }

    fn rule(kind: RuleKind, matcher: RuleMatcher) -> AccessRule {
        AccessRule { kind, matcher }
    }

    #[test]
    fn empty_allow_list_denies() {
        let decision = evaluate(&[], &peer("192.168.1.10", None));
        assert_eq!(decision, PolicyDecision::Deny);

        let allow_mac = vec![rule(
            RuleKind::Allow,
            RuleMatcher::Mac("aa:bb:cc:dd:ee:ff".to_owned()),
        )];
        assert_eq!(
            evaluate_rules(&allow_mac, &peer("192.168.1.10", None)).expect("evaluate"),
            PolicyDecision::Deny,
            "a MAC allow rule cannot match without a MAC"
        );
    }

    #[test]
    fn deny_wins_over_allow() {
        let rules = vec![
            rule(
                RuleKind::Allow,
                RuleMatcher::Network("192.168.1.0/24".to_owned()),
            ),
            rule(RuleKind::Deny, RuleMatcher::Ip("192.168.1.10".to_owned())),
        ];
        assert_eq!(
            evaluate_rules(&rules, &peer("192.168.1.10", None)).expect("evaluate"),
            PolicyDecision::Deny
        );
        assert_eq!(
            evaluate_rules(&rules, &peer("192.168.1.11", None)).expect("evaluate"),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn mac_rules_require_a_known_mac_but_ip_rules_stay_independent() {
        let rules = vec![
            rule(
                RuleKind::Deny,
                RuleMatcher::Mac("aa:bb:cc:dd:ee:ff".to_owned()),
            ),
            rule(RuleKind::Allow, RuleMatcher::Ip("192.168.1.10".to_owned())),
        ];

        assert_eq!(
            evaluate_rules(&rules, &peer("192.168.1.10", None)).expect("evaluate"),
            PolicyDecision::Allow,
            "an unmatched MAC deny rule must not block an independent IP allow rule"
        );
        assert_eq!(
            evaluate_rules(&rules, &peer("192.168.1.10", Some("aa:bb:cc:dd:ee:ff")))
                .expect("evaluate"),
            PolicyDecision::Deny
        );
    }

    #[test]
    fn ipv4_mapped_peers_match_ipv4_rules() {
        let rules = vec![rule(
            RuleKind::Allow,
            RuleMatcher::Network("192.168.1.0/24".to_owned()),
        )];
        assert_eq!(
            evaluate_rules(&rules, &peer("::ffff:192.168.1.10", None)).expect("evaluate"),
            PolicyDecision::Allow
        );
        assert_eq!(
            canonical_ip("::ffff:10.0.0.1".parse().expect("ip")),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))
        );
        assert_eq!(
            canonical_ip("2001:db8::1".parse::<Ipv6Addr>().expect("ip").into()),
            "2001:db8::1".parse::<IpAddr>().expect("ip")
        );
    }

    #[test]
    fn mac_normalization_accepts_common_forms() {
        let expected = [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
        assert_eq!(parse_mac("AA:BB:CC:DD:EE:FF").expect("mac"), expected);
        assert_eq!(parse_mac("aa-bb-cc-dd-ee-ff").expect("mac"), expected);
        assert_eq!(parse_mac("aabbccddeeff").expect("mac"), expected);
        assert_eq!(format_mac(&expected), "aa:bb:cc:dd:ee:ff");
        assert!(parse_mac("aa:bb:cc:dd:ee").is_err());
        assert!(parse_mac("zz:bb:cc:dd:ee:ff").is_err());
    }

    #[test]
    fn invalid_rules_are_rejected() {
        assert!(
            compile_rule(&rule(
                RuleKind::Allow,
                RuleMatcher::Ip("not-an-ip".to_owned())
            ))
            .is_err()
        );
        assert!(
            compile_rule(&rule(
                RuleKind::Allow,
                RuleMatcher::Network("192.168.1.0/33".to_owned())
            ))
            .is_err()
        );
        assert!(compile_rule(&rule(RuleKind::Allow, RuleMatcher::Mac("nope".to_owned()))).is_err());
    }

    #[test]
    fn normalization_canonicalizes_and_deduplicates() {
        let rules = vec![
            rule(
                RuleKind::Allow,
                RuleMatcher::Network("192.168.1.10/24".to_owned()),
            ),
            rule(
                RuleKind::Allow,
                RuleMatcher::Network("192.168.1.0/24".to_owned()),
            ),
            rule(
                RuleKind::Allow,
                RuleMatcher::Mac("AA-BB-CC-DD-EE-FF".to_owned()),
            ),
        ];
        let normalized = normalize_rules(&rules).expect("normalize");
        assert_eq!(normalized.len(), 2);
        assert_eq!(
            normalized[0].matcher,
            RuleMatcher::Network("192.168.1.0/24".to_owned())
        );
        assert_eq!(
            normalized[1].matcher,
            RuleMatcher::Mac("aa:bb:cc:dd:ee:ff".to_owned())
        );
    }

    #[test]
    fn method_permission_mapping_is_conservative() {
        let read_only = SharePermissions {
            can_read: true,
            can_write: false,
            can_delete: false,
        };
        let write_only = SharePermissions {
            can_read: false,
            can_write: true,
            can_delete: false,
        };
        let delete_only = SharePermissions {
            can_read: false,
            can_write: false,
            can_delete: true,
        };

        let get = required_for_method(&http::Method::GET).expect("GET");
        assert!(get.is_met_by(read_only));
        assert!(!get.is_met_by(write_only));

        let put = required_for_method(&http::Method::PUT).expect("PUT");
        assert!(put.is_met_by(write_only));
        assert!(!put.is_met_by(read_only));

        let delete = required_for_method(&http::Method::DELETE).expect("DELETE");
        assert!(delete.is_met_by(delete_only));
        assert!(!delete.is_met_by(read_only));

        let copy =
            required_for_method(&http::Method::from_bytes(b"COPY").expect("COPY")).expect("COPY");
        assert!(copy.is_met_by(SharePermissions {
            can_read: true,
            can_write: true,
            can_delete: false,
        }));
        assert!(!copy.is_met_by(read_only));

        let mv = http::Method::from_bytes(b"MOVE").expect("MOVE");
        let mov = required_for_method(&mv).expect("MOVE");
        assert!(mov.is_met_by(SharePermissions {
            can_read: false,
            can_write: true,
            can_delete: true,
        }));

        let proppatch = http::Method::from_bytes(b"PROPPATCH").expect("PROPPATCH");
        assert!(required_for_method(&proppatch).is_none());
    }
}
