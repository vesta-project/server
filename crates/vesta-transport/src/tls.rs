//! Bounded, explicit TLS reloads. A single snapshot owns config and discovery identity.
//!
//! `vesta-agent serve` calls `Transport::reload_tls` every five seconds, serially.
//! Each poll reads both configured PEM paths with the existing protected reader
//! (256 KiB per file, rejecting symlinks/reparse points), then observes mtime,
//! size and SHA-256. Content is hashed even when metadata is unchanged. Invalid
//! candidates remain retryable on the next poll; only a validated complete pair
//! replaces the live snapshot. There is no plaintext or generated-cert fallback.
//!
//! Install a complete chain and matching private key at the configured paths,
//! preferably by replacing each regular file rather than writing it in place.
//! A mismatched intermediate installation keeps the previous snapshot. Filesystem
//! observations inherit the reader's point-in-time, not race-free, guarantees.
//! Calls are synchronous and bounded in bytes, not by a filesystem IO deadline.
//!
//! Discovery (`POST /api/v2/discovery`) exposes the currently active leaf DER
//! SHA-256 as 64 lowercase hex characters in `X-Vesta-TLS-SHA256-Fingerprint`.
//! The strict versioned JSON manifest is deliberately unchanged. The header is
//! descriptive, not an independent trust grant; clients must validate TLS first.
//! An existing connection can report the new listener fingerprint while its TLS
//! stream still uses the old certificate. Accepted handshakes retain their snapshot;
//! connections accepted after a successful reload use the replacement configuration.
//!
//! Every certificate's DER and the private key are parsed, and key/leaf consistency
//! must be proven. The existing rustls public API has no standalone validity-window
//! accessor; server-side expiry/not-before and issuer-chain validation are not done.
//! Clients still perform ordinary certificate validity and chain verification.
//! Operators must install currently valid material with an appropriate trust chain.
//! Reload failures record only `tls_material_unavailable`, with no paths or bytes.
use crate::TransportError;
use rustls_pki_types::pem::PemObject;
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::SystemTime,
};
use vesta_control::{config::TlsListener, host::read_tls_document};

type Identity = (Option<SystemTime>, u64, [u8; 32]);
pub(crate) struct Snapshot {
    certified: rustls::sign::CertifiedKey,
    config: Arc<rustls::ServerConfig>,
    fingerprint: String,
    documents: (Identity, Identity),
}
pub(crate) struct State {
    current: Snapshot,
    last_error: Option<TransportError>,
}
pub(crate) struct Material {
    listener: TlsListener,
    reload: Mutex<()>,
    pub(crate) state: Mutex<State>,
}
impl Material {
    /// Certificate, fingerprint and signature come from one TLS snapshot.
    pub(crate) fn sign_pack(
        &self,
        value: &mut serde_json::Value,
    ) -> Result<serde_json::Value, TransportError> {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let state = self.lock()?;
        value["fingerprint"] = state.current.fingerprint.clone().into();
        let payload = serde_json::to_vec(value).map_err(|_| TransportError::TlsMaterial)?;
        let mut message = b"vesta-pack-sync-v1\0".to_vec();
        message.extend_from_slice(&payload);
        let signer = state
            .current
            .certified
            .key
            .choose_scheme(&[
                rustls::SignatureScheme::ED25519,
                rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
                rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
                rustls::SignatureScheme::RSA_PSS_SHA256,
            ])
            .ok_or(TransportError::TlsMaterial)?;
        let signature = signer
            .sign(&message)
            .map_err(|_| TransportError::TlsMaterial)?;
        Ok(
            serde_json::json!({"payload_base64":STANDARD.encode(payload),"certificate_base64":STANDARD.encode(state.current.certified.cert[0].as_ref()),"signature_base64":STANDARD.encode(signature)}),
        )
    }
    pub(crate) fn new(listener: &TlsListener) -> Result<Self, TransportError> {
        Ok(Self {
            listener: listener.clone(),
            reload: Mutex::new(()),
            state: Mutex::new(State {
                current: load(listener)?,
                last_error: None,
            }),
        })
    }
    pub(crate) fn config(&self) -> Result<Arc<rustls::ServerConfig>, TransportError> {
        Ok(self.lock()?.current.config.clone())
    }
    pub(crate) fn fingerprint(&self) -> Result<String, TransportError> {
        Ok(self.lock()?.current.fingerprint.clone())
    }
    pub(crate) fn last_error(&self) -> Option<TransportError> {
        self.lock()
            .map_or(Some(TransportError::TlsMaterial), |s| s.last_error)
    }
    // Serialize reloads so an older read cannot overwrite a newer successful reload.
    // The protected reader caps each document at 256 KiB. File reads and parsing run
    // under the reload lock only; the state lock (taken by every accepted handshake)
    // is held just to swap the validated snapshot. Established TLS streams own their
    // original configuration.
    pub(crate) fn reload(&self) -> Result<bool, TransportError> {
        self.reload_with(load)
    }
    pub(crate) fn reload_with(
        &self,
        load: impl FnOnce(&TlsListener) -> Result<Snapshot, TransportError>,
    ) -> Result<bool, TransportError> {
        let _serial = self
            .reload
            .lock()
            .map_err(|_| TransportError::TlsMaterial)?;
        let next = load(&self.listener);
        let mut state = self.lock()?;
        match next {
            Ok(next) => {
                let changed = next.documents != state.current.documents;
                if changed {
                    state.current = next;
                }
                state.last_error = None;
                Ok(changed)
            }
            Err(_) => {
                state.last_error = Some(TransportError::TlsMaterial);
                Err(TransportError::TlsMaterial)
            }
        }
    }
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>, TransportError> {
        self.state.lock().map_err(|_| TransportError::TlsMaterial)
    }
}
fn document(path: &Path) -> Result<(zeroize::Zeroizing<Vec<u8>>, Identity), TransportError> {
    let bytes =
        zeroize::Zeroizing::new(read_tls_document(path).map_err(|_| TransportError::TlsMaterial)?);
    let metadata = std::fs::metadata(path).map_err(|_| TransportError::TlsMaterial)?;
    let identity = (
        metadata.modified().ok(),
        bytes.len() as u64,
        Sha256::digest(&*bytes).into(),
    );
    Ok((bytes, identity))
}
pub(crate) fn load(listener: &TlsListener) -> Result<Snapshot, TransportError> {
    let (certificates, certificate_identity) = document(listener.certificate())?;
    let (key, key_identity) = document(listener.private_key())?;
    if !complete_pem(&certificates, "CERTIFICATE", true)
        || !["PRIVATE KEY", "RSA PRIVATE KEY", "EC PRIVATE KEY"]
            .iter()
            .any(|label| complete_pem(&key, label, false))
    {
        return Err(TransportError::TlsMaterial);
    }
    let certificates = rustls_pki_types::CertificateDer::pem_slice_iter(&certificates)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| TransportError::TlsMaterial)?;
    let leaf = certificates.first().ok_or(TransportError::TlsMaterial)?;
    // Validate every DER certificate, not just PEM framing. rustls exposes no
    // standalone validity-window accessor; expiry remains client-validated.
    for certificate in &certificates {
        rustls::server::ParsedCertificate::try_from(certificate)
            .map_err(|_| TransportError::TlsMaterial)?;
    }
    let fingerprint = Sha256::digest(leaf.as_ref())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let key = rustls_pki_types::PrivateKeyDer::from_pem_slice(&key)
        .map_err(|_| TransportError::TlsMaterial)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let certified = rustls::sign::CertifiedKey::from_der(certificates, key, &provider)
        .map_err(|_| TransportError::TlsMaterial)?;
    // Reject unknown consistency too: never swap a pair without proving a match.
    certified
        .keys_match()
        .map_err(|_| TransportError::TlsMaterial)?;
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| TransportError::TlsMaterial)?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(rustls::sign::SingleCertAndKey::from(
            certified.clone(),
        )));
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Snapshot {
        certified,
        config: Arc::new(config),
        fingerprint,
        documents: (certificate_identity, key_identity),
    })
}

// PEM iterators may skip unrelated text or an unfinished trailing block. Reject
// those documents rather than accepting the complete prefix of a half-written chain.
fn complete_pem(bytes: &[u8], label: &str, multiple: bool) -> bool {
    let Ok(mut text) = std::str::from_utf8(bytes).map(str::trim) else {
        return false;
    };
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let mut count = 0;
    while !text.is_empty() {
        let Some(rest) = text.strip_prefix(&begin) else {
            return false;
        };
        let Some(position) = rest.find(&end) else {
            return false;
        };
        if rest[..position].contains("-----") {
            return false;
        }
        text = rest[position + end.len()..].trim();
        count += 1;
        if !multiple && count > 1 {
            return false;
        }
    }
    count != 0
}
