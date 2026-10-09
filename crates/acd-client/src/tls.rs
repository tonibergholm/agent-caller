//! Client side of decision D1: verify the server's pinned SPKI and key proof.
use std::sync::Arc;

use acd_protocol::pairing::{pins_equal, spki_pin};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{
    CryptoProvider, WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature,
};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error, SignatureScheme};

pub const PIN_MISMATCH: &str = "acd: server key does not match the pairing pin";

#[derive(Debug)]
pub struct PinVerifier {
    pin: String,
    algs: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let (_, cert) = x509_parser::parse_x509_certificate(cert.as_ref())
            .map_err(|_| Error::General(PIN_MISMATCH.into()))?;
        if pins_equal(&spki_pin(cert.tbs_certificate.subject_pki.raw), &self.pin) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(Error::General(PIN_MISMATCH.into()))
        }
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(m, c, d, &self.algs)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(m, c, d, &self.algs)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

pub fn client_config(pin: &str) -> Arc<rustls::ClientConfig> {
    let provider: Arc<CryptoProvider> = Arc::new(rustls::crypto::ring::default_provider());
    let algs = provider.signature_verification_algorithms;
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("TLS 1.3 is supported by ring")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinVerifier {
            pin: pin.to_string(),
            algs,
        }))
        .with_no_client_auth();
    Arc::new(cfg)
}

pub fn is_pin_mismatch(e: &std::io::Error) -> bool {
    e.to_string().contains(PIN_MISMATCH)
}
