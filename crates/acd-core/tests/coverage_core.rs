//! Coverage-driven tests (docs/research/2026-11-05-coverage.md).
use acd_core::tls::{check_pair, generate, pin_of_cert_pem, server_config, KeyFiles, TlsError};

fn pem_err(r: Result<KeyFiles, TlsError>) -> String {
    match r {
        Err(TlsError::Pem(m)) => m,
        Err(e) => panic!("wrong error kind: {e}"),
        Ok(_) => panic!("accepted"),
    }
}

/// A bundle is exactly one key and one certificate and nothing else: the
/// daemon will not start from a file that carries extra or hidden content.
#[test]
fn a_bundle_with_extra_stray_or_broken_parts_is_refused() {
    let a = generate("a");
    let b = generate("b");
    let ok = a.to_bundle();
    let parsed = KeyFiles::from_bundle(&ok).unwrap();
    assert_eq!(parsed.to_bundle(), ok);

    assert!(pem_err(KeyFiles::from_bundle(&format!("{ok}{}", b.cert_pem))).contains("exactly one"));
    assert!(pem_err(KeyFiles::from_bundle(&format!("{ok}{}", b.key_pem))).contains("exactly one"));
    assert!(pem_err(KeyFiles::from_bundle(&a.cert_pem)).contains("exactly one"));
    assert!(pem_err(KeyFiles::from_bundle("")).contains("exactly one"));
    assert!(pem_err(KeyFiles::from_bundle(&format!("note\n{ok}"))).contains("outside"));
    let cut = &ok[..ok.len() - 20];
    assert!(pem_err(KeyFiles::from_bundle(cut)).contains("unterminated"));
    let nested = format!("-----BEGIN CERTIFICATE-----\n{ok}");
    assert!(pem_err(KeyFiles::from_bundle(&nested)).contains("nested"));
    let other = "-----BEGIN DH PARAMETERS-----\nAAAA\n-----END DH PARAMETERS-----\n";
    assert!(pem_err(KeyFiles::from_bundle(&format!("{ok}{other}"))).contains("unexpected"));
    // A block whose body is not valid base64 DER is refused at parse time.
    let junk = "-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n";
    assert!(matches!(
        KeyFiles::from_bundle(&format!("{}{junk}", a.key_pem)),
        Err(TlsError::Pem(_))
    ));
}

/// The pin is derived from the certificate; a key that does not belong to it
/// is refused before any listener is built.
#[test]
fn a_mismatched_key_and_certificate_never_become_a_server_config() {
    let a = generate("a");
    let b = generate("b");
    let pin = check_pair(&a).unwrap();
    assert_eq!(pin_of_cert_pem(&a.cert_pem).unwrap(), pin);
    assert_ne!(pin_of_cert_pem(&b.cert_pem).unwrap(), pin);

    let crossed = KeyFiles { key_pem: a.key_pem.clone(), cert_pem: b.cert_pem.clone() };
    assert!(matches!(check_pair(&crossed), Err(TlsError::Cert(m)) if m.contains("does not match")));
    assert!(server_config(&crossed).is_err());
    assert!(server_config(&a).is_ok());

    assert!(matches!(pin_of_cert_pem("not a pem"), Err(TlsError::Pem(_))));
    let no_key = KeyFiles { key_pem: String::new(), cert_pem: a.cert_pem.clone() };
    assert!(check_pair(&no_key).is_err());
    assert!(server_config(&no_key).is_err());
}
