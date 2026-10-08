use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use std::fmt;
use subtle::ConstantTimeEq;

pub fn spki_pin(spki_der: &[u8]) -> String {
    format!(
        "sha256:{}",
        URL_SAFE_NO_PAD.encode(Sha256::digest(spki_der))
    )
}

pub fn pins_equal(a: &str, b: &str) -> bool {
    let (Some(a), Some(b)) = (decode_pin(a), decode_pin(b)) else {
        return false;
    };
    bool::from(a.ct_eq(&b))
}

fn decode_pin(pin: &str) -> Option<[u8; 32]> {
    let encoded = pin.strip_prefix("sha256:")?;
    if encoded.len() != 43 {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(encoded).ok()?;
    if bytes.len() != 32 || URL_SAFE_NO_PAD.encode(&bytes) != encoded {
        return None;
    }
    bytes.try_into().ok()
}

fn valid_token(token: &str) -> bool {
    token.len() == 43
        && URL_SAFE_NO_PAD
            .decode(token)
            .is_ok_and(|b| b.len() == 32 && URL_SAFE_NO_PAD.encode(b) == token)
}

#[derive(Clone, PartialEq, Eq)]
pub struct Pairing {
    pub host: String,
    pub port: u16,
    pub pin: String,
    pub token: String,
}

impl fmt::Debug for Pairing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pairing")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("pin", &self.pin)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PairingError {
    #[error("invalid pairing scheme")]
    Scheme,
    #[error("invalid pairing host")]
    Host,
    #[error("invalid pairing port")]
    Port,
    #[error("invalid pairing pin")]
    Pin,
    #[error("invalid pairing token")]
    Token,
}

impl Pairing {
    pub fn parse(s: &str) -> Result<Pairing, PairingError> {
        let s = s.trim();
        let rest = s.strip_prefix("acd://").ok_or(PairingError::Scheme)?;
        let (address, token) = rest.split_once('#').ok_or(PairingError::Token)?;
        if token.contains('#') || !valid_token(token) {
            return Err(PairingError::Token);
        }
        let (address, pin) = address.split_once("?pin=").ok_or(PairingError::Pin)?;
        if pin.contains('?') || decode_pin(pin).is_none() {
            return Err(PairingError::Pin);
        }
        let (host, port) = if let Some(ipv6) = address.strip_prefix('[') {
            let (host, tail) = ipv6.split_once(']').ok_or(PairingError::Host)?;
            let port = tail.strip_prefix(':').ok_or(PairingError::Port)?;
            (host, port)
        } else {
            address.rsplit_once(':').ok_or(PairingError::Port)?
        };
        if host.is_empty()
            || host
                .chars()
                .any(|c| c.is_whitespace() || matches!(c, '/' | '@' | '?' | '#' | '[' | ']'))
        {
            return Err(PairingError::Host);
        }
        let port = port
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .ok_or(PairingError::Port)?;
        Ok(Pairing {
            host: host.into(),
            port,
            pin: pin.into(),
            token: token.into(),
        })
    }

    pub fn to_line(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("acd://{host}:{}?pin={}#{}", self.port, self.pin, self.token)
    }
}
