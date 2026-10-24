//! The router's own certificates, built by hand in DER and signed with
//! ring's ECDSA P-256 (ring is already linked for TLS; a certificate crate
//! would add its own DER and time code), plus a small DER reader for what
//! the agent needs back from its files.
//!
//! - The certificate authority (`authority`): a self-signed **root** (what
//!   devices trust) that signs exactly one **issuing CA**; the root's key
//!   (p256, wiped when dropped) lives only inside `authority`, never on
//!   disk. Both carry `basicConstraints` CA (critical; `pathLen` 1 and 0),
//!   `keyUsage` keyCertSign + cRLSign (critical), an extended key usage of
//!   serverAuth only (Windows, Chrome and NSS then refuse them for code
//!   signing or e-mail), and critical **name constraints**: what the caller
//!   permits (the router's LAN address as a /32 and the local name), every
//!   IPv6 address excluded, and every IPv4 address too when no IPv4 network
//!   is permitted (no subtree of a type would leave that type open).
//!   Why two: some verifiers ignore name constraints in a trust anchor the
//!   user installed (Android's platform verifier, which Chrome on Android
//!   uses: tested, docs/web-ui.md), but every one enforces them in an
//!   intermediate certificate. The agent keeps only the issuing CA's key,
//!   so a leaked key can vouch for nothing outside the constraints on any
//!   device. The constraints, not `pathLen`, are what hold: a CA the
//!   issuing key makes for itself (self-issued, which RFC 5280 does not
//!   count against `pathLen`) inherits them.
//! - The server certificate (`leaf`, signed by the issuing CA):
//!   `basicConstraints` CA:FALSE and `keyUsage` digitalSignature (both
//!   critical), serverAuth, and its names as subject alternative names (DNS
//!   names and IP addresses). The server sends it with the issuing CA.

use std::net::{IpAddr, Ipv4Addr};

use p256::ecdsa::signature::Signer;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};

use crate::web::sms::{civil, days_from_civil};

// Object identifiers (content octets only).
const OID_EC_PUBLIC_KEY: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const OID_P256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
const OID_ECDSA_SHA256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];
const OID_CN: &[u8] = &[0x55, 0x04, 0x03];
const OID_ORG: &[u8] = &[0x55, 0x04, 0x0a];
pub const OID_BASIC: &[u8] = &[0x55, 0x1d, 0x13];
pub const OID_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x0f];
pub const OID_EKU: &[u8] = &[0x55, 0x1d, 0x25];
pub const OID_SAN: &[u8] = &[0x55, 0x1d, 0x11];
pub const OID_NAME_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x1e];
pub const OID_SKI: &[u8] = &[0x55, 0x1d, 0x0e];
pub const OID_AKI: &[u8] = &[0x55, 0x1d, 0x23];
pub const OID_SERVER_AUTH: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];

const BOOL: u8 = 0x01;
const INT: u8 = 0x02;
const BITS: u8 = 0x03;
const OCTETS: u8 = 0x04;
const OID: u8 = 0x06;
const UTF8: u8 = 0x0c;
const UTC_TIME: u8 = 0x17;
const GEN_TIME: u8 = 0x18;
const SEQ: u8 = 0x30;
const SET: u8 = 0x31;
/// GeneralName choices (context-specific, primitive).
const GN_DNS: u8 = 0x82;
const GN_IP: u8 = 0x87;

/// A private key (PKCS#8 DER) and its certificate (DER).
#[derive(Clone)]
pub struct KeyCert {
    pub key: Vec<u8>,
    pub cert: Vec<u8>,
}

/// A CA certificate: its name, validity (unix seconds) and what it may
/// vouch for.
pub struct CaSpec<'a> {
    pub cn: &'a str,
    pub not_before: i64,
    pub not_after: i64,
    /// Permitted IPv4 networks (address, prefix length).
    pub ips: &'a [(Ipv4Addr, u8)],
    /// Permitted DNS zones (the name and every name below it).
    pub dns: &'a [&'a str],
}

/// A server certificate: its DNS names (the first is also its common name),
/// its addresses and validity.
pub struct LeafSpec<'a> {
    pub dns: &'a [&'a str],
    pub ips: &'a [IpAddr],
    pub not_before: i64,
    pub not_after: i64,
}

pub type Error = &'static str;

// --- Writing ---

fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len() + 6);
    out.push(tag);
    let n = content.len();
    if n < 0x80 {
        out.push(n as u8);
    } else {
        let be = n.to_be_bytes();
        let skip = be.iter().take_while(|b| **b == 0).count();
        out.push(0x80 | (be.len() - skip) as u8);
        out.extend_from_slice(&be[skip..]);
    }
    out.extend_from_slice(content);
    out
}

fn seq(parts: &[&[u8]]) -> Vec<u8> {
    tlv(SEQ, &parts.concat())
}

/// A non-negative INTEGER from big-endian bytes (minimal, with a leading
/// zero when the top bit is set).
fn uint(bytes: &[u8]) -> Vec<u8> {
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    let b = &bytes[start..];
    match b.first() {
        None => tlv(INT, &[0]),
        Some(f) if f & 0x80 != 0 => tlv(INT, &[&[0u8][..], b].concat()),
        Some(_) => tlv(INT, b),
    }
}

fn bits(b: &[u8]) -> Vec<u8> {
    tlv(BITS, &[&[0u8][..], b].concat())
}

/// UTCTime through 2049, GeneralizedTime after (RFC 5280 4.1.2.5).
fn time(t: i64) -> Vec<u8> {
    let (days, s) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    let (y, m, d) = civil(days);
    let hms = format!("{:02}{:02}{:02}Z", s / 3600, s / 60 % 60, s % 60);
    if (1950..2050).contains(&y) {
        tlv(UTC_TIME, format!("{:02}{m:02}{d:02}{hms}", y % 100).as_bytes())
    } else {
        tlv(GEN_TIME, format!("{y:04}{m:02}{d:02}{hms}").as_bytes())
    }
}

fn name(org: Option<&str>, cn: &str) -> Vec<u8> {
    let rdn = |oid: &[u8], v: &str| tlv(SET, &seq(&[&tlv(OID, oid), &tlv(UTF8, v.as_bytes())]));
    let mut parts = org.map(|o| rdn(OID_ORG, o)).unwrap_or_default();
    parts.extend(rdn(OID_CN, cn));
    tlv(SEQ, &parts)
}

fn ext(oid: &[u8], critical: bool, value: &[u8]) -> Vec<u8> {
    let crit = if critical { tlv(BOOL, &[0xff]) } else { Vec::new() };
    seq(&[&tlv(OID, oid), &crit, &tlv(OCTETS, value)])
}

fn spki(point: &[u8]) -> Vec<u8> {
    seq(&[&seq(&[&tlv(OID, OID_EC_PUBLIC_KEY), &tlv(OID, OID_P256)]), &bits(point)])
}

/// A key identifier: the leftmost 160 bits of the SHA-256 of the public
/// key (RFC 7093, method 1).
fn key_id(point: &[u8]) -> Vec<u8> {
    ring::digest::digest(&ring::digest::SHA256, point).as_ref()[..20].to_vec()
}

fn ip_octets(ip: &IpAddr) -> Vec<u8> {
    match ip {
        IpAddr::V4(a) => a.octets().to_vec(),
        IpAddr::V6(a) => a.octets().to_vec(),
    }
}

/// The name constraints' value: the permitted IPv4 networks and DNS names
/// (each with every name below it); every IPv6 address excluded (`::/0`),
/// so a verifier that treats IPv6 as a name type of its own still refuses
/// one, and every IPv4 address (`0.0.0.0/0`) when none is permitted (a
/// name type without a permitted subtree is otherwise unconstrained).
fn name_constraints(ips: &[(Ipv4Addr, u8)], dns: &[&str]) -> Vec<u8> {
    let mut permitted = Vec::new();
    for (net, len) in ips {
        let mask = u32::MAX.checked_shl(32 - u32::from((*len).min(32))).unwrap_or(0);
        permitted.extend(seq(&[&tlv(GN_IP, &[net.octets(), mask.to_be_bytes()].concat())]));
    }
    for zone in dns {
        permitted.extend(seq(&[&tlv(GN_DNS, zone.as_bytes())]));
    }
    let mut excluded = Vec::new();
    if ips.is_empty() {
        excluded.extend(seq(&[&tlv(GN_IP, &[0u8; 8])]));
    }
    excluded.extend(seq(&[&tlv(GN_IP, &[0u8; 32])]));
    seq(&[&tlv(0xa0, &permitted), &tlv(0xa1, &excluded)])
}

/// What a CA may vouch for: its name constraints' permitted IPv4 networks
/// and DNS names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scope {
    pub ips: Vec<(Ipv4Addr, u8)>,
    pub dns: Vec<String>,
}

impl Scope {
    pub fn covers_ip(&self, ip: Ipv4Addr) -> bool {
        self.ips.iter().any(|(net, len)| {
            let keep = u32::MAX.checked_shl(32 - u32::from((*len).min(32))).unwrap_or(0);
            u32::from(ip) & keep == u32::from(*net) & keep
        })
    }

    /// The name or a name below one permitted (RFC 5280 4.2.1.10).
    pub fn covers_name(&self, name: &str) -> bool {
        self.dns.iter().any(|z| name == z || name.strip_suffix(z.as_str()).is_some_and(|l| l.ends_with('.') && l.len() > 1))
    }
}

/// The permitted subtrees of a CA certificate's name constraints; `None`
/// without them (or unreadable).
pub fn scope(info: &Info) -> Option<Scope> {
    let (_, nc) = info.ext(OID_NAME_CONSTRAINTS)?;
    let (body, _) = expect(nc, SEQ)?;
    let (mut permitted, _) = expect(body, 0xa0)?;
    let mut out = Scope::default();
    while let Some((_, subtree, more)) = next(permitted) {
        permitted = more;
        match next(subtree)? {
            (GN_IP, b, _) if b.len() == 8 => {
                let mask = u32::from_be_bytes([b[4], b[5], b[6], b[7]]);
                out.ips.push((Ipv4Addr::new(b[0], b[1], b[2], b[3]), mask.leading_ones() as u8));
            }
            (GN_DNS, b, _) => out.dns.push(String::from_utf8_lossy(b).to_ascii_lowercase()),
            _ => {}
        }
    }
    Some(out)
}

fn serial(rng: &SystemRandom) -> Result<Vec<u8>, Error> {
    let mut s = [0u8; 16];
    rng.fill(&mut s).map_err(|_| "no randomness")?;
    // Positive and exactly 16 bytes long.
    s[0] = (s[0] & 0x7f) | 0x40;
    Ok(uint(&s))
}

fn new_key(rng: &SystemRandom) -> Result<(Vec<u8>, EcdsaKeyPair), Error> {
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, rng).map_err(|_| "key generation failed")?;
    let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), rng).map_err(|_| "bad key")?;
    Ok((pkcs8.as_ref().to_vec(), pair))
}

#[allow(clippy::too_many_arguments)]
fn tbs(serial: &[u8], issuer: &[u8], not_before: i64, not_after: i64, subject: &[u8], point: &[u8], exts: &[Vec<u8>]) -> Vec<u8> {
    let version = tlv(0xa0, &tlv(INT, &[2]));
    let alg = seq(&[&tlv(OID, OID_ECDSA_SHA256)]);
    let validity = seq(&[&time(not_before), &time(not_after)]);
    let exts = tlv(0xa3, &tlv(SEQ, &exts.concat()));
    seq(&[&version, serial, &alg, issuer, &validity, subject, &spki(point), &exts])
}

/// Who signs a certificate: a stored CA key (ring) or the root's (p256,
/// wiped when dropped).
enum SignWith<'a> {
    Ring(&'a EcdsaKeyPair, &'a SystemRandom),
    P256(&'a p256::ecdsa::SigningKey),
}

fn signed(signer: SignWith, tbs: Vec<u8>) -> Result<Vec<u8>, Error> {
    let sig = match signer {
        SignWith::Ring(pair, rng) => pair.sign(rng, &tbs).map_err(|_| "signing failed")?.as_ref().to_vec(),
        SignWith::P256(key) => {
            let sig: p256::ecdsa::DerSignature = key.sign(&tbs);
            sig.as_bytes().to_vec()
        }
    };
    let alg = seq(&[&tlv(OID, OID_ECDSA_SHA256)]);
    Ok(seq(&[&tbs, &alg, &bits(&sig)]))
}

const ORG: &str = "OpenU60 zte-agent";

/// A CA certificate's extensions: cA with `path_len`, keyCertSign +
/// cRLSign, serverAuth, the name constraints, its key id and (below the
/// root) its issuer's.
fn ca_exts(spec: &CaSpec, path_len: u8, id: &[u8], authority: Option<&[u8]>) -> Vec<Vec<u8>> {
    let mut exts = vec![
        ext(OID_BASIC, true, &seq(&[&tlv(BOOL, &[0xff]), &tlv(INT, &[path_len])])),
        // keyCertSign (bit 5) and cRLSign (bit 6); one unused bit.
        ext(OID_KEY_USAGE, true, &tlv(BITS, &[0x01, 0x06])),
        ext(OID_EKU, false, &seq(&[&tlv(OID, OID_SERVER_AUTH)])),
        ext(OID_NAME_CONSTRAINTS, true, &name_constraints(spec.ips, spec.dns)),
        ext(OID_SKI, false, &tlv(OCTETS, id)),
    ];
    if let Some(a) = authority {
        exts.push(ext(OID_AKI, false, &seq(&[&tlv(0x80, a)])));
    }
    exts
}

/// The router's certificate authority: the root's certificate (what
/// devices trust; its key is dropped here, after it signed the issuing
/// CA) and the issuing CA's key and certificate.
pub fn authority(root: &CaSpec, issuing: &CaSpec) -> Result<(Vec<u8>, KeyCert), Error> {
    let rng = SystemRandom::new();
    // The root key lives only in this function (p256's key wipes itself
    // when dropped; ring's does not).
    let root_key = p256::ecdsa::SigningKey::random(&mut p256::elliptic_curve::rand_core::OsRng);
    let root_point = root_key.verifying_key().to_encoded_point(false);
    let root_point = root_point.as_bytes();
    let root_name = name(Some(ORG), root.cn);
    let root_id = key_id(root_point);
    // pathLen 1: one CA below it, the issuing CA.
    let exts = ca_exts(root, 1, &root_id, None);
    let root_cert = signed(SignWith::P256(&root_key), tbs(&serial(&rng)?, &root_name, root.not_before, root.not_after, &root_name, root_point, &exts))?;
    let (key, pair) = new_key(&rng)?;
    let point = pair.public_key().as_ref();
    // pathLen 0: no CA below it (the constraints hold either way).
    let exts = ca_exts(issuing, 0, &key_id(point), Some(&root_id));
    let subject = name(Some(ORG), issuing.cn);
    let cert = signed(SignWith::P256(&root_key), tbs(&serial(&rng)?, &root_name, issuing.not_before, issuing.not_after, &subject, point, &exts))?;
    drop(root_key);
    Ok((root_cert, KeyCert { key, cert }))
}

/// A new server key and its certificate, signed by `ca` (the issuing CA).
pub fn leaf(ca: &KeyCert, spec: &LeafSpec) -> Result<KeyCert, Error> {
    let rng = SystemRandom::new();
    let ca_info = parse(&ca.cert).ok_or("unreadable CA certificate")?;
    let ca_pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &ca.key, &rng).map_err(|_| "unreadable CA key")?;
    if ca_pair.public_key().as_ref() != ca_info.point.as_slice() {
        return Err("CA key does not match its certificate");
    }
    let (key, pair) = new_key(&rng)?;
    let point = pair.public_key().as_ref();
    let cn = spec.dns.first().copied().unwrap_or("zte-agent");
    let mut san = Vec::new();
    for d in spec.dns {
        san.extend(tlv(GN_DNS, d.as_bytes()));
    }
    for ip in spec.ips {
        san.extend(tlv(GN_IP, &ip_octets(ip)));
    }
    let aki = seq(&[&tlv(0x80, &ca_info.key_id.clone().unwrap_or_else(|| key_id(&ca_info.point)))]);
    let exts = [
        ext(OID_BASIC, true, &seq(&[])),
        // digitalSignature (bit 0); seven unused bits.
        ext(OID_KEY_USAGE, true, &tlv(BITS, &[0x07, 0x80])),
        ext(OID_EKU, false, &seq(&[&tlv(OID, OID_SERVER_AUTH)])),
        ext(OID_SAN, false, &tlv(SEQ, &san)),
        ext(OID_SKI, false, &tlv(OCTETS, &key_id(point))),
        ext(OID_AKI, false, &aki),
    ];
    let subject = name(None, cn);
    let tbs = tbs(&serial(&rng)?, &ca_info.subject, spec.not_before, spec.not_after, &subject, point, &exts);
    Ok(KeyCert { cert: signed(SignWith::Ring(&ca_pair, &rng), tbs)?, key })
}

/// The public key (uncompressed point) of a PKCS#8 P-256 key, `None` if it
/// is not one.
pub fn key_point(pkcs8: &[u8]) -> Option<Vec<u8>> {
    let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8, &SystemRandom::new()).ok()?;
    Some(pair.public_key().as_ref().to_vec())
}

// --- Reading ---

/// One DER element: its tag, content and what follows it.
fn next(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let n = usize::from(first & 0x7f);
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        (rest[..n].iter().fold(0usize, |a, &b| (a << 8) | usize::from(b)), &rest[n..])
    };
    (rest.len() >= len).then(|| (tag, &rest[..len], &rest[len..]))
}

/// The next element if it has `tag`.
fn expect(input: &[u8], tag: u8) -> Option<(&[u8], &[u8])> {
    next(input).filter(|(t, ..)| *t == tag).map(|(_, c, r)| (c, r))
}

fn parse_time(tag: u8, s: &[u8]) -> Option<i64> {
    let s = std::str::from_utf8(s).ok()?.strip_suffix('Z')?;
    let (y, rest) = match tag {
        UTC_TIME => {
            let yy: i64 = s.get(..2)?.parse().ok()?;
            (if yy < 50 { 2000 + yy } else { 1900 + yy }, s.get(2..)?)
        }
        GEN_TIME => (s.get(..4)?.parse().ok()?, s.get(4..)?),
        _ => return None,
    };
    if rest.len() != 10 || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = |i: usize| rest[i..i + 2].parse::<i64>().ok();
    let (mo, d, h, mi, sec) = (n(0)?, n(2)?, n(4)?, n(6)?, n(8)?);
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec)
}

/// What the agent reads back from a certificate.
#[derive(Debug, Clone)]
pub struct Info {
    /// The subject as encoded (a leaf's issuer must be byte for byte this).
    pub subject: Vec<u8>,
    /// The issuer as encoded.
    pub issuer: Vec<u8>,
    pub cn: String,
    pub not_before: i64,
    pub not_after: i64,
    /// The public key (uncompressed point).
    pub point: Vec<u8>,
    pub key_id: Option<Vec<u8>>,
    pub dns: Vec<String>,
    pub ips: Vec<IpAddr>,
    /// Every extension: OID, critical, value.
    pub exts: Vec<(Vec<u8>, bool, Vec<u8>)>,
}

impl Info {
    pub fn ext(&self, oid: &[u8]) -> Option<(bool, &[u8])> {
        self.exts.iter().find(|(o, ..)| o == oid).map(|(_, c, v)| (*c, v.as_slice()))
    }
}

/// A certificate's subject, validity, key and names; `None` if it is not
/// one this module could have written.
pub fn parse(cert: &[u8]) -> Option<Info> {
    let (cert, _) = expect(cert, SEQ)?;
    let (tbs, _) = expect(cert, SEQ)?;
    let rest = match next(tbs)? {
        (0xa0, _, rest) => rest,
        _ => tbs,
    };
    let (_serial, rest) = expect(rest, INT)?;
    let (_alg, at_issuer) = expect(rest, SEQ)?;
    let (_issuer, rest) = expect(at_issuer, SEQ)?;
    let issuer_raw = at_issuer[..at_issuer.len() - rest.len()].to_vec();
    let (validity, rest) = expect(rest, SEQ)?;
    let subject_len = rest.len();
    let (subject, after) = expect(rest, SEQ)?;
    let subject_raw = rest[..subject_len - after.len()].to_vec();
    let (spki, mut rest) = expect(after, SEQ)?;
    let (t1, nb, v) = next(validity)?;
    let (t2, na, _) = next(v)?;
    let (_alg, spki_rest) = expect(spki, SEQ)?;
    let (point, _) = expect(spki_rest, BITS)?;
    let mut info = Info {
        subject: subject_raw,
        issuer: issuer_raw,
        cn: common_name(subject).unwrap_or_default(),
        not_before: parse_time(t1, nb)?,
        not_after: parse_time(t2, na)?,
        point: point.get(1..)?.to_vec(),
        key_id: None,
        dns: Vec::new(),
        ips: Vec::new(),
        exts: Vec::new(),
    };
    while let Some((tag, content, more)) = next(rest) {
        rest = more;
        if tag != 0xa3 {
            continue;
        }
        let (mut list, _) = expect(content, SEQ)?;
        while let Some((_, e, more)) = next(list) {
            list = more;
            let (oid, e) = expect(e, OID)?;
            let (critical, e) = match expect(e, BOOL) {
                Some((b, r)) => (b == [0xff], r),
                None => (false, e),
            };
            let (value, _) = expect(e, OCTETS)?;
            info.exts.push((oid.to_vec(), critical, value.to_vec()));
        }
    }
    if let Some((_, v)) = info.ext(OID_SKI) {
        info.key_id = expect(v, OCTETS).map(|(id, _)| id.to_vec());
    }
    if let Some((_, v)) = info.ext(OID_SAN) {
        let (mut names, _) = expect(v, SEQ)?;
        let (mut dns, mut ips) = (Vec::new(), Vec::new());
        while let Some((tag, n, more)) = next(names) {
            names = more;
            match (tag, n.len()) {
                (GN_DNS, _) => dns.push(String::from_utf8_lossy(n).into_owned()),
                (GN_IP, 4) => ips.push(IpAddr::from(<[u8; 4]>::try_from(n).ok()?)),
                (GN_IP, 16) => ips.push(IpAddr::from(<[u8; 16]>::try_from(n).ok()?)),
                _ => {}
            }
        }
        (info.dns, info.ips) = (dns, ips);
    }
    Some(info)
}

fn common_name(subject: &[u8]) -> Option<String> {
    let mut rdns = subject;
    while let Some((_, set, more)) = next(rdns) {
        rdns = more;
        let (atv, _) = expect(set, SEQ)?;
        let (oid, value) = expect(atv, OID)?;
        if oid == OID_CN {
            return next(value).map(|(_, v, _)| String::from_utf8_lossy(v).into_owned());
        }
    }
    None
}

/// "AB:CD:…" of `digest(der)`.
fn hex_pairs(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

/// The SHA-256 fingerprint as devices show it ("AB:CD:…").
pub fn sha256_fingerprint(der: &[u8]) -> String {
    hex_pairs(ring::digest::digest(&ring::digest::SHA256, der).as_ref())
}

/// The SHA-1 fingerprint (Windows' "Thumbprint").
pub fn sha1_fingerprint(der: &[u8]) -> String {
    hex_pairs(ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, der).as_ref())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use rustls::client::danger::ServerCertVerifier;
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use std::sync::Arc;

    pub const NOW: i64 = 1_793_000_000; // 2026-10-26
    pub const DAY: i64 = 86_400;

    pub fn private_v4() -> [(Ipv4Addr, u8); 3] {
        [(Ipv4Addr::new(10, 0, 0, 0), 8), (Ipv4Addr::new(172, 16, 0, 0), 12), (Ipv4Addr::new(192, 168, 0, 0), 16)]
    }

    /// The root's certificate and the issuing CA.
    pub struct Ca {
        pub root: Vec<u8>,
        pub issuing: KeyCert,
    }

    pub fn test_authority() -> Ca {
        let ips = private_v4();
        let spec = |cn| CaSpec { cn, not_before: NOW - 2 * DAY, not_after: NOW + 3650 * DAY, ips: &ips, dns: &["home.arpa"] };
        let (root, issuing) = authority(&spec("U60 Pro local CA 0123ABCD"), &spec("U60 Pro issuing CA 0123ABCD")).unwrap();
        Ca { root, issuing }
    }

    pub fn test_leaf(issuing: &KeyCert, dns: &[&str], ips: &[&str]) -> KeyCert {
        let ips: Vec<IpAddr> = ips.iter().map(|s| s.parse().unwrap()).collect();
        leaf(issuing, &LeafSpec { dns, ips: &ips, not_before: NOW - 2 * DAY, not_after: NOW + 395 * DAY }).unwrap()
    }

    /// webpki (rustls' verifier) with `root` as the only trust anchor (its
    /// name constraints come along: `anchor_from_trusted_cert`) and
    /// `chain` as the server sends it, leaf first.
    pub fn verify(root: &[u8], chain: &[&[u8]], name: &str, at: i64) -> Result<(), rustls::Error> {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from(root.to_vec())).unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let v = rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider).build().unwrap();
        let name = ServerName::try_from(name.to_owned()).unwrap();
        let at = UnixTime::since_unix_epoch(std::time::Duration::from_secs(at as u64));
        let certs: Vec<CertificateDer> = chain.iter().map(|c| CertificateDer::from(c.to_vec())).collect();
        v.verify_server_cert(&certs[0], &certs[1..], &name, &[], at).map(|_| ())
    }

    #[test]
    fn der_primitives() {
        assert_eq!(uint(&[0x00, 0x00, 0x7f]), [0x02, 0x01, 0x7f]);
        assert_eq!(uint(&[0x80]), [0x02, 0x02, 0x00, 0x80]);
        assert_eq!(uint(&[]), [0x02, 0x01, 0x00]);
        assert_eq!(tlv(0x04, &[0u8; 200])[..3], [0x04, 0x81, 200]);
        assert_eq!(tlv(0x04, &[0u8; 300])[..4], [0x04, 0x82, 0x01, 0x2c]);
        assert_eq!(next(&tlv(0x04, &[7u8; 300])).map(|(t, c, r)| (t, c.len(), r.len())), Some((0x04, 300, 0)));
        assert_eq!(next(&[0x04, 0x05, 1, 2]), None);
        // 2026-10-24 12:34:56 UTC; 2050 needs GeneralizedTime.
        let t = days_from_civil(2026, 10, 24) * DAY + 12 * 3600 + 34 * 60 + 56;
        assert_eq!(time(t), tlv(UTC_TIME, b"261024123456Z"));
        assert_eq!(parse_time(UTC_TIME, b"261024123456Z"), Some(t));
        let t2050 = days_from_civil(2050, 1, 1) * DAY;
        assert_eq!(time(t2050), tlv(GEN_TIME, b"20500101000000Z"));
        assert_eq!(parse_time(GEN_TIME, b"20500101000000Z"), Some(t2050));
        assert_eq!(parse_time(UTC_TIME, b"2610241234Z"), None);
    }

    /// The constraints' subtrees (tag, base) and the excluded part.
    fn subtrees(nc: &[u8]) -> (Vec<(u8, Vec<u8>)>, Vec<u8>) {
        let (body, _) = expect(nc, SEQ).unwrap();
        let (permitted, rest) = expect(body, 0xa0).unwrap();
        let (excluded, _) = expect(rest, 0xa1).unwrap();
        let mut out = Vec::new();
        let mut p = permitted;
        while let Some((_, st, more)) = next(p) {
            p = more;
            let (tag, base, _) = next(st).unwrap();
            out.push((tag, base.to_vec()));
        }
        (out, excluded.to_vec())
    }

    /// The root and the issuing CA as the design says: cA with pathLen 1
    /// and 0, keyCertSign + cRLSign, serverAuth only, the same name
    /// constraints in both, present and critical (the private IPv4 ranges
    /// and home.arpa permitted, IPv6 excluded), ten years; the issuing CA
    /// signed by the root, whose key is gone.
    #[test]
    fn authority_extensions() {
        let ca = test_authority();
        let (root, issuing) = (parse(&ca.root).unwrap(), parse(&ca.issuing.cert).unwrap());
        assert_eq!((root.cn.as_str(), issuing.cn.as_str()), ("U60 Pro local CA 0123ABCD", "U60 Pro issuing CA 0123ABCD"));
        assert_eq!(root.issuer, root.subject);
        assert_eq!(issuing.issuer, root.subject);
        for (info, path_len) in [(&root, 1u8), (&issuing, 0)] {
            assert_eq!((info.not_before, info.not_after), (NOW - 2 * DAY, NOW + 3650 * DAY));
            assert_eq!(info.ext(OID_BASIC), Some((true, &[0x30, 0x06, 0x01, 0x01, 0xff, 0x02, 0x01, path_len][..])));
            assert_eq!(info.ext(OID_KEY_USAGE), Some((true, &[0x03, 0x02, 0x01, 0x06][..])));
            assert_eq!(info.ext(OID_EKU).map(|(c, v)| (c, v.to_vec())), Some((false, seq(&[&tlv(OID, OID_SERVER_AUTH)]))));
            let (critical, nc) = info.ext(OID_NAME_CONSTRAINTS).unwrap();
            assert!(critical);
            let (permitted, excluded) = subtrees(nc);
            assert_eq!(
                permitted,
                [
                    (GN_IP, vec![10, 0, 0, 0, 255, 0, 0, 0]),
                    (GN_IP, vec![172, 16, 0, 0, 255, 240, 0, 0]),
                    (GN_IP, vec![192, 168, 0, 0, 255, 255, 0, 0]),
                    (GN_DNS, b"home.arpa".to_vec()),
                ]
            );
            assert_eq!(excluded, seq(&[&tlv(GN_IP, &[0u8; 32])]));
            assert_eq!(info.key_id.as_deref(), Some(&key_id(&info.point)[..]));
        }
        assert!(root.ext(OID_AKI).is_none());
        assert_eq!(issuing.ext(OID_AKI).map(|(_, v)| v.to_vec()), Some(seq(&[&tlv(0x80, root.key_id.as_ref().unwrap())])));
        // The only key is the issuing CA's.
        assert_eq!(key_point(&ca.issuing.key), Some(issuing.point.clone()));
        assert_ne!(issuing.point, root.point);
        // The issuing CA verifies under the root (as a CA it is no server
        // certificate, but the signature and name chaining are checked).
        assert!(ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_ASN1, &root.point)
            .verify(tbs_of(&ca.issuing.cert).0, tbs_of(&ca.issuing.cert).1)
            .is_ok());
    }

    /// (TBSCertificate, signature) of a certificate.
    fn tbs_of(cert: &[u8]) -> (&[u8], &[u8]) {
        let (c, _) = expect(cert, SEQ).unwrap();
        let (_, after) = expect(c, SEQ).unwrap();
        let tbs = &c[..c.len() - after.len()];
        let (_, after) = expect(after, SEQ).unwrap();
        let (sig, _) = expect(after, BITS).unwrap();
        (tbs, &sig[1..])
    }

    #[test]
    fn leaf_extensions_and_names() {
        let ca = test_authority();
        let l = test_leaf(&ca.issuing, &["u60.home.arpa"], &["192.168.0.1"]);
        let i = parse(&l.cert).unwrap();
        assert_eq!(i.cn, "u60.home.arpa");
        assert_eq!(i.issuer, parse(&ca.issuing.cert).unwrap().subject);
        assert_eq!(i.dns, ["u60.home.arpa"]);
        assert_eq!(i.ips, [IpAddr::from([192, 168, 0, 1])]);
        assert_eq!(i.not_after - i.not_before, 397 * DAY);
        assert_eq!(i.ext(OID_BASIC), Some((true, &[0x30, 0x00][..])));
        assert_eq!(i.ext(OID_KEY_USAGE), Some((true, &[0x03, 0x02, 0x07, 0x80][..])));
        assert!(i.ext(OID_EKU).is_some_and(|(c, v)| !c && v == seq(&[&tlv(OID, OID_SERVER_AUTH)])));
        assert!(i.ext(OID_NAME_CONSTRAINTS).is_none());
        let issuing_id = parse(&ca.issuing.cert).unwrap().key_id.unwrap();
        assert_eq!(i.ext(OID_AKI).map(|(_, v)| v.to_vec()), Some(seq(&[&tlv(0x80, &issuing_id)])));
        assert_eq!(key_point(&l.key), Some(i.point));
        // Fingerprints as devices show them.
        let fp = sha256_fingerprint(&ca.root);
        assert_eq!((fp.len(), fp.matches(':').count()), (95, 31));
        assert_eq!(sha1_fingerprint(&ca.root).len(), 59);
    }

    /// The chain verifies for the address and the name, and fails for a
    /// name or address outside the constraints even though the issuing CA
    /// signed it, for an IPv6 address, before notBefore and after notAfter,
    /// and without the issuing CA in the chain.
    #[test]
    fn chain_verifies_only_inside_the_constraints() {
        let ca = test_authority();
        let l = test_leaf(&ca.issuing, &["u60.home.arpa"], &["192.168.0.1"]);
        let chain = |leaf: &KeyCert| [leaf.cert.clone(), ca.issuing.cert.clone()];
        let ok = chain(&l);
        let ok: Vec<&[u8]> = ok.iter().map(Vec::as_slice).collect();
        verify(&ca.root, &ok, "192.168.0.1", NOW).unwrap();
        verify(&ca.root, &ok, "u60.home.arpa", NOW).unwrap();
        assert!(verify(&ca.root, &ok, "192.168.0.2", NOW).is_err());
        assert!(verify(&ca.root, &ok, "u60.home.arpa", NOW - 3 * DAY).is_err());
        assert!(verify(&ca.root, &ok, "u60.home.arpa", NOW + 396 * DAY).is_err());
        assert!(verify(&ca.root, &ok[..1], "u60.home.arpa", NOW).is_err());
        for (dns, ip, name) in [
            (&["example.com"][..], &[][..], "example.com"),
            (&["www.google.com"], &[], "www.google.com"),
            (&["home.arpa.evil.example"], &[], "home.arpa.evil.example"),
            (&[], &["8.8.8.8"], "8.8.8.8"),
            (&[], &["127.0.0.1"], "127.0.0.1"),
            (&[], &["172.32.0.1"], "172.32.0.1"),
            (&[], &["2001:db8::1"], "2001:db8::1"),
            // In range by one name, out by the other: the whole certificate is refused.
            (&["u60.home.arpa", "example.com"], &["192.168.0.1"], "u60.home.arpa"),
        ] {
            let bad = test_leaf(&ca.issuing, dns, ip);
            let c = chain(&bad);
            let c: Vec<&[u8]> = c.iter().map(Vec::as_slice).collect();
            let e = verify(&ca.root, &c, name, NOW).unwrap_err();
            assert!(format!("{e:?}").contains("NameConstraint"), "{dns:?} {ip:?}: {e:?}");
        }
        // Inside: any home.arpa name, any private address.
        let inside = test_leaf(&ca.issuing, &["nas.home.arpa"], &["10.1.2.3", "172.31.255.254"]);
        let c = chain(&inside);
        let c: Vec<&[u8]> = c.iter().map(Vec::as_slice).collect();
        for name in ["nas.home.arpa", "10.1.2.3", "172.31.255.254"] {
            verify(&ca.root, &c, name, NOW).unwrap();
        }
        // Another authority's server certificate is not trusted.
        let other = test_authority();
        let o = test_leaf(&other.issuing, &["u60.home.arpa"], &[]);
        assert!(verify(&ca.root, &[&o.cert, &other.issuing.cert], "u60.home.arpa", NOW).is_err());
    }

    /// What Android's platform verifier does (it ignores a user-installed
    /// root's constraints): the issuing CA's own constraints still refuse
    /// a name outside them. Here the root has none at all.
    #[test]
    fn the_issuing_cas_constraints_hold_without_the_roots() {
        let rng = SystemRandom::new();
        let ips = private_v4();
        let spec = CaSpec { cn: "test root without constraints", not_before: NOW - DAY, not_after: NOW + DAY, ips: &ips, dns: &["home.arpa"] };
        let (_, root_pair) = new_key(&rng).unwrap();
        let root_point = root_pair.public_key().as_ref();
        let root_name = name(None, spec.cn);
        let root_id = key_id(root_point);
        let exts: Vec<Vec<u8>> = ca_exts(&spec, 1, &root_id, None).into_iter().filter(|e| e[2..7] != [0x06, 0x03, 0x55, 0x1d, 0x1e]).collect();
        assert_eq!(exts.len(), 4);
        let root = signed(SignWith::Ring(&root_pair, &rng), tbs(&serial(&rng).unwrap(), &root_name, NOW - DAY, NOW + DAY, &root_name, root_point, &exts)).unwrap();
        let (key, pair) = new_key(&rng).unwrap();
        let point = pair.public_key().as_ref();
        let exts = ca_exts(&spec, 0, &key_id(point), Some(&root_id));
        let subject = name(None, "test issuing CA");
        let cert = signed(SignWith::Ring(&root_pair, &rng), tbs(&serial(&rng).unwrap(), &root_name, NOW - DAY, NOW + DAY, &subject, point, &exts)).unwrap();
        let issuing = KeyCert { key, cert };
        let good = test_leaf(&issuing, &["u60.home.arpa"], &[]);
        verify(&root, &[&good.cert, &issuing.cert], "u60.home.arpa", NOW).unwrap();
        let bad = test_leaf(&issuing, &["example.com"], &[]);
        let e = verify(&root, &[&bad.cert, &issuing.cert], "example.com", NOW).unwrap_err();
        assert!(format!("{e:?}").contains("NameConstraint"), "{e:?}");
    }

    /// The router's scope (owner decision 59): one address as a /32 and
    /// one name. Only those verify; another private address or another
    /// `home.arpa` name, signed with the issuing key, is refused (what a
    /// stolen key could otherwise vouch for). The scope reads back.
    #[test]
    fn a_narrow_scope_vouches_for_the_router_only() {
        let ips = [(Ipv4Addr::new(192, 168, 0, 1), 32)];
        let spec = |cn| CaSpec { cn, not_before: NOW - 2 * DAY, not_after: NOW + 3650 * DAY, ips: &ips, dns: &["u60.home.arpa"] };
        let (root, issuing) = authority(&spec("U60 Pro local CA 0123ABCD"), &spec("U60 Pro issuing CA 0123ABCD")).unwrap();
        for info in [parse(&root).unwrap(), parse(&issuing.cert).unwrap()] {
            let sc = scope(&info).unwrap();
            assert_eq!(sc, Scope { ips: vec![(Ipv4Addr::new(192, 168, 0, 1), 32)], dns: vec!["u60.home.arpa".into()] });
            assert!(sc.covers_ip(Ipv4Addr::new(192, 168, 0, 1)) && !sc.covers_ip(Ipv4Addr::new(192, 168, 0, 2)));
            assert!(sc.covers_name("u60.home.arpa") && sc.covers_name("x.u60.home.arpa"));
            assert!(!sc.covers_name("nas.home.arpa") && !sc.covers_name("xu60.home.arpa") && !sc.covers_name("home.arpa"));
            let (_, nc) = info.ext(OID_NAME_CONSTRAINTS).unwrap();
            assert_eq!(subtrees(nc).1, seq(&[&tlv(GN_IP, &[0u8; 32])]), "IPv6 excluded, IPv4 limited by the /32");
        }
        let good = test_leaf(&issuing, &["u60.home.arpa"], &["192.168.0.1"]);
        for name in ["u60.home.arpa", "192.168.0.1"] {
            verify(&root, &[&good.cert, &issuing.cert], name, NOW).unwrap();
        }
        for (dns, ip, name) in [
            (&["nas.home.arpa"][..], &["192.168.5.5"][..], "nas.home.arpa"),
            (&[], &["192.168.5.5"], "192.168.5.5"),
            (&[], &["192.168.0.2"], "192.168.0.2"),
            (&[], &["10.20.30.40"], "10.20.30.40"),
            (&["u60.home.arpa"], &["192.168.1.1"], "u60.home.arpa"),
            (&["example.com"], &[], "example.com"),
        ] {
            let bad = test_leaf(&issuing, dns, ip);
            let e = verify(&root, &[&bad.cert, &issuing.cert], name, NOW).unwrap_err();
            assert!(format!("{e:?}").contains("NameConstraint"), "{dns:?} {ip:?}: {e:?}");
        }
    }

    /// A CA made without any IPv4 network (a LAN address outside the
    /// private ranges) excludes every IPv4 address instead of leaving IPv4
    /// unconstrained; its name still verifies.
    #[test]
    fn no_permitted_address_excludes_every_address() {
        let spec = |cn| CaSpec { cn, not_before: NOW - 2 * DAY, not_after: NOW + 3650 * DAY, ips: &[], dns: &["u60.home.arpa"] };
        let (root, issuing) = authority(&spec("U60 Pro local CA 00000000"), &spec("U60 Pro issuing CA 00000000")).unwrap();
        let (_, nc) = parse(&issuing.cert).unwrap().ext(OID_NAME_CONSTRAINTS).map(|(c, v)| (c, v.to_vec())).unwrap();
        assert_eq!(subtrees(&nc).1, [seq(&[&tlv(GN_IP, &[0u8; 8])]), seq(&[&tlv(GN_IP, &[0u8; 32])])].concat());
        assert_eq!(scope(&parse(&root).unwrap()).unwrap().ips, []);
        let good = test_leaf(&issuing, &["u60.home.arpa"], &[]);
        verify(&root, &[&good.cert, &issuing.cert], "u60.home.arpa", NOW).unwrap();
        for ip in ["100.64.0.1", "192.168.0.1", "8.8.8.8"] {
            let bad = test_leaf(&issuing, &[], &[ip]);
            assert!(verify(&root, &[&bad.cert, &issuing.cert], ip, NOW).is_err(), "{ip}");
        }
    }

    #[test]
    fn leaf_refuses_a_mismatched_ca_key() {
        let (a, b) = (test_authority(), test_authority());
        let mixed = KeyCert { key: b.issuing.key.clone(), cert: a.issuing.cert.clone() };
        let spec = LeafSpec { dns: &["u60.home.arpa"], ips: &[], not_before: NOW, not_after: NOW + DAY };
        assert_eq!(leaf(&mixed, &spec).err(), Some("CA key does not match its certificate"));
        assert!(parse(b"\x30\x03\x02\x01\x00").is_none());
        assert!(key_point(b"junk").is_none());
    }
}
