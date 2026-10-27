//! Installation-scoped mutual TLS. Enrollment accepts a self-signed peer only
//! after validating its certificate and handshake signature; resource access is
//! authorized separately against the fingerprint of that TLS peer. Broadcasts
//! are address hints, never trust anchors.

use std::path::Path;
use std::sync::Arc;

use base64::Engine;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::error::DomainError;
use crate::types::DeviceIdentity;
use crate::vault::Vault;

fn tls_error(operation: &'static str, error: impl std::fmt::Display) -> DomainError {
    DomainError::Crypto {
        operation,
        detail: error.to_string(),
    }
}

pub(crate) fn fingerprint(cert: &CertificateDer<'_>) -> Result<String, DomainError> {
    let (_, parsed) = x509_parser::parse_x509_certificate(cert.as_ref())
        .map_err(|error| tls_error("parse device certificate", error))?;
    Ok(hex::encode(Sha256::digest(parsed.public_key().raw)))
}

fn issue_certificate(
    key: &rcgen::KeyPair,
    now: time::OffsetDateTime,
) -> Result<CertificateDer<'static>, DomainError> {
    let mut params = rcgen::CertificateParams::new(vec!["peerdav.local".into()])
        .map_err(|error| tls_error("configure device certificate", error))?;
    params.not_before = now - time::Duration::minutes(5);
    params.not_after = now
        .replace_year(now.year() + 10)
        .or_else(|_| (now - time::Duration::days(1)).replace_year(now.year() + 10))
        .map_err(|error| tls_error("set certificate expiration", error))?;
    params.extended_key_usages = vec![
        rcgen::ExtendedKeyUsagePurpose::ServerAuth,
        rcgen::ExtendedKeyUsagePurpose::ClientAuth,
    ];
    let cert = params
        .self_signed(key)
        .map_err(|error| tls_error("sign device certificate", error))?;
    Ok(cert.der().clone())
}

fn identity_payload(
    cert: &CertificateDer<'_>,
    key: &PrivatePkcs8KeyDer<'_>,
) -> Result<Zeroizing<Vec<u8>>, DomainError> {
    serde_json::to_vec(&(
        base64::engine::general_purpose::STANDARD.encode(cert.as_ref()),
        base64::engine::general_purpose::STANDARD.encode(key.secret_pkcs8_der()),
    ))
    .map(Zeroizing::new)
    .map_err(|source| DomainError::Serialization {
        operation: "encode device identity",
        source,
    })
}

fn persist_identity(
    path: &Path,
    cert: &CertificateDer<'_>,
    key: &PrivatePkcs8KeyDer<'_>,
) -> Result<(), DomainError> {
    use std::io::Write;
    let mut nonce = [0u8; 8];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut nonce);
    let temporary = path.with_extension(format!("{}.tmp", hex::encode(nonce)));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temporary)?;
        file.write_all(&identity_payload(cert, key).map_err(std::io::Error::other)?)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if let Err(source) = result {
        if let Err(cleanup) = std::fs::remove_file(&temporary)
            && cleanup.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("failed to remove identity staging file: {cleanup}");
        }
        return Err(DomainError::Io {
            operation: "persist device identity",
            path: Some(path.into()),
            source,
        });
    }
    Ok(())
}

pub(crate) fn load_identity(database: &Path) -> Result<DeviceIdentity, DomainError> {
    static IDENTITY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = IDENTITY_LOCK
        .lock()
        .map_err(|_| tls_error("load device identity", "identity lock poisoned"))?;
    let path = database.with_extension("identity.json");
    let now = time::OffsetDateTime::now_utc();
    let (mut cert, key) = if path.exists() {
        let bytes = Zeroizing::new(std::fs::read(&path).map_err(|source| DomainError::Io {
            operation: "read device identity",
            path: Some(path.clone()),
            source,
        })?);
        let pair: (String, Zeroizing<String>) = serde_json::from_slice::<(String, String)>(&bytes)
            .map(|(cert, key)| (cert, Zeroizing::new(key)))
            .map_err(|source| DomainError::Serialization {
                operation: "decode device identity",
                source,
            })?;
        (
            CertificateDer::from(
                base64::engine::general_purpose::STANDARD
                    .decode(pair.0)
                    .map_err(|error| tls_error("decode device certificate", error))?,
            ),
            PrivatePkcs8KeyDer::from(
                base64::engine::general_purpose::STANDARD
                    .decode(pair.1.as_bytes())
                    .map_err(|error| tls_error("decode device key", error))?,
            ),
        )
    } else {
        let key =
            rcgen::KeyPair::generate().map_err(|error| tls_error("generate device key", error))?;
        let cert = issue_certificate(&key, now)?;
        let key = PrivatePkcs8KeyDer::from(key.serialize_der());
        persist_identity(&path, &cert, &key)?;
        (cert, key)
    };
    // Detect broken key/certificate pairs before attempting renewal.
    rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|error| tls_error("configure TLS", error))?
        .with_no_client_auth()
        .with_single_cert(vec![cert.clone()], key.clone_key().into())
        .map_err(|error| tls_error("validate device identity", error))?;
    let (_, parsed) = x509_parser::parse_x509_certificate(cert.as_ref())
        .map_err(|error| tls_error("parse certificate expiration", error))?;
    if parsed.validity().not_after.timestamp() <= (now + time::Duration::days(30)).unix_timestamp()
    {
        let key_pair =
            rcgen::KeyPair::try_from(&key).map_err(|error| tls_error("load renewal key", error))?;
        cert = issue_certificate(&key_pair, now)?;
        persist_identity(&path, &cert, &key)?;
    }
    Ok(DeviceIdentity {
        fingerprint: fingerprint(&cert)?,
        certificate: cert,
        key,
    })
}

pub(crate) fn normalize_fingerprint(value: Option<&str>) -> Option<String> {
    let value = value?.trim().to_ascii_lowercase();
    (value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(value)
}

pub(crate) fn short_fingerprint(value: &str) -> String {
    if value.is_ascii() && value.len() > 16 {
        format!("{}…{}", &value[..8], &value[value.len() - 4..])
    } else {
        value.to_owned()
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn self_signed_client(
    cert: &CertificateDer<'_>,
    intermediates: &[CertificateDer<'_>],
    now: UnixTime,
) -> Result<ClientCertVerified, rustls::Error> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.clone())?;
    let verifier =
        rustls::server::WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider())
            .build()
            .map_err(|error| rustls::Error::General(error.to_string()))?;
    verifier.verify_client_cert(cert, intermediates, now)
}

#[derive(Debug)]
struct EnrollmentClient;
impl ClientCertVerifier for EnrollmentClient {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }
    fn verify_client_cert(
        &self,
        cert: &CertificateDer<'_>,
        chain: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        self_signed_client(cert, chain, now)
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            signature,
            &provider().signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            signature,
            &provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[derive(Debug)]
struct PinnedServer {
    expected: String,
}
impl ServerCertVerifier for PinnedServer {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        chain: &[CertificateDer<'_>],
        _name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if fingerprint(cert).map_err(|error| rustls::Error::General(error.to_string()))?
            != self.expected
        {
            return Err(rustls::Error::General(
                "server identity does not match the paired fingerprint".into(),
            ));
        }
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert.clone())?;
        let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots),
            provider(),
        )
        .build()
        .map_err(|error| rustls::Error::General(error.to_string()))?;
        let name = ServerName::try_from("peerdav.local")
            .map_err(|error| rustls::Error::General(error.to_string()))?;
        verifier.verify_server_cert(cert, chain, &name, ocsp, now)
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            signature,
            &provider().signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            signature,
            &provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub(crate) fn acceptor(
    identity: &DeviceIdentity,
) -> Result<tokio_rustls::TlsAcceptor, DomainError> {
    let mut config = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|error| tls_error("configure TLS server", error))?
        .with_client_cert_verifier(Arc::new(EnrollmentClient))
        .with_single_cert(
            vec![identity.certificate.clone()],
            identity.key.clone_key().into(),
        )
        .map_err(|error| tls_error("configure device certificate", error))?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
}

pub(crate) fn self_connection_allowed(local: &str, remote: &str) -> bool {
    cfg!(debug_assertions) || local != remote
}

pub(crate) fn agent(
    vault: &Vault,
    expected: &str,
    timeout: std::time::Duration,
) -> Result<reqwest::Client, DomainError> {
    if !self_connection_allowed(vault.fingerprint(), expected) {
        return Err(DomainError::SelfConnectionForbidden);
    }
    let identity = vault.current_identity()?;
    let mut config = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|error| tls_error("configure TLS client", error))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedServer {
            expected: expected.into(),
        }))
        .with_client_auth_cert(
            vec![identity.certificate.clone()],
            identity.key.clone_key().into(),
        )
        .map_err(|error| tls_error("configure client identity", error))?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    reqwest::Client::builder()
        .use_preconfigured_tls(config)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .map_err(|error| tls_error("build encrypted client", error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renewed_certificate_keeps_the_pin_and_mutual_tls_validation() {
        let key = rcgen::KeyPair::generate().expect("key");
        let now = time::OffsetDateTime::now_utc();
        let original = issue_certificate(&key, now - time::Duration::days(4000)).expect("expired");
        let renewed = issue_certificate(&key, now).expect("renewed");
        let pin = fingerprint(&original).expect("original pin");
        let verifier = PinnedServer { expected: pin };
        let name = ServerName::try_from("192.168.1.23").expect("name");
        assert!(
            verifier
                .verify_server_cert(&renewed, &[], &name, &[], UnixTime::now())
                .is_ok()
        );
        assert!(self_signed_client(&renewed, &[], UnixTime::now()).is_ok());
        assert!(
            verifier
                .verify_server_cert(&original, &[], &name, &[], UnixTime::now())
                .is_err()
        );
        let other = rcgen::KeyPair::generate().expect("other device");
        let impostor = issue_certificate(&other, now).expect("different cert");
        assert!(
            verifier
                .verify_server_cert(&impostor, &[], &name, &[], UnixTime::now())
                .is_err()
        );
    }

    #[test]
    fn self_connections_follow_build_mode_not_network_address() {
        assert_eq!(
            self_connection_allowed("same", "same"),
            cfg!(debug_assertions)
        );
        assert!(self_connection_allowed("local", "different"));
    }

    #[test]
    fn self_client_is_rejected_only_in_release_builds() {
        let dir = tempfile::tempdir().expect("directory");
        let vault = Vault::open(&dir.path().join("vault.db")).expect("vault");
        let result = agent(
            &vault,
            vault.fingerprint(),
            std::time::Duration::from_secs(1),
        );
        if cfg!(debug_assertions) {
            assert!(result.is_ok());
        } else {
            assert!(matches!(result, Err(DomainError::SelfConnectionForbidden)));
        }
    }

    #[test]
    fn near_expiry_renews_but_corrupt_identity_is_not_replaced() {
        let dir = tempfile::tempdir().expect("directory");
        let database = dir.path().join("vault.db");
        let path = database.with_extension("identity.json");
        let key = rcgen::KeyPair::generate().expect("key");
        let now = time::OffsetDateTime::now_utc();
        let mut params =
            rcgen::CertificateParams::new(vec!["peerdav.local".into()]).expect("params");
        params.not_before = now - time::Duration::days(1);
        params.not_after = now + time::Duration::days(20);
        let cert = params.self_signed(&key).expect("certificate");
        persist_identity(
            &path,
            cert.der(),
            &PrivatePkcs8KeyDer::from(key.serialize_der()),
        )
        .expect("persist");
        let renewed = load_identity(&database).expect("renew");
        assert_ne!(&renewed.certificate, cert.der());
        assert_eq!(
            renewed.fingerprint,
            fingerprint(cert.der()).expect("fingerprint")
        );
        std::fs::write(&path, b"broken identity").expect("corrupt fixture");
        assert!(load_identity(&database).is_err());
        assert_eq!(std::fs::read(&path).expect("unchanged"), b"broken identity");
    }

    #[test]
    fn installation_identity_is_persistent_and_certificate_lasts_ten_years() {
        let dir = tempfile::tempdir().expect("directory");
        let database = dir.path().join("vault.db");
        let first = load_identity(&database).expect("identity");
        let again = load_identity(&database).expect("identity again");
        assert_eq!(first.fingerprint, again.fingerprint);
        assert_eq!(first.certificate, again.certificate);
        let (_, parsed) =
            x509_parser::parse_x509_certificate(first.certificate.as_ref()).expect("certificate");
        let remaining = parsed.validity().not_after.timestamp()
            - time::OffsetDateTime::now_utc().unix_timestamp();
        assert!(remaining > 3650 * 86400 && remaining < 3654 * 86400);
    }

    #[test]
    fn expired_certificate_is_renewed_with_the_original_identity() {
        let dir = tempfile::tempdir().expect("directory");
        let database = dir.path().join("vault.db");
        let key = rcgen::KeyPair::generate().expect("key");
        let old = issue_certificate(
            &key,
            time::OffsetDateTime::now_utc() - time::Duration::days(4000),
        )
        .expect("expired certificate");
        let original = fingerprint(&old).expect("fingerprint");
        persist_identity(
            &database.with_extension("identity.json"),
            &old,
            &PrivatePkcs8KeyDer::from(key.serialize_der()),
        )
        .expect("persist");
        let renewed = load_identity(&database).expect("renew");
        assert_ne!(renewed.certificate, old);
        assert_eq!(renewed.fingerprint, original);
        let (_, parsed) = x509_parser::parse_x509_certificate(renewed.certificate.as_ref())
            .expect("renewed certificate");
        assert!(parsed.validity().is_valid());
    }
}
