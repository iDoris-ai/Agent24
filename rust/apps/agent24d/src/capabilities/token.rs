use rand::RngCore;

use super::types::{CapabilityClaims, CapabilityError};

/// Opaque 256-bit bearer secret. It has no debug, display, or serialization.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct CapabilityToken(pub(crate) [u8; 32]);

impl CapabilityToken {
    pub(crate) fn random() -> Self {
        let mut bytes = [0_u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub(crate) fn bearer(&self) -> String {
        let mut bearer = String::with_capacity(64);
        for byte in self.0 {
            use std::fmt::Write as _;
            let _ = write!(bearer, "{byte:02x}");
        }
        bearer
    }

    pub fn parse_bearer(bearer: &str) -> Result<Self, CapabilityError> {
        if bearer.len() != 64 || !bearer.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(CapabilityError::Unauthorized);
        }
        let mut bytes = [0_u8; 32];
        for (index, pair) in bearer.as_bytes().chunks_exact(2).enumerate() {
            bytes[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
        }
        Ok(Self(bytes))
    }
}

fn hex_nibble(byte: u8) -> Result<u8, CapabilityError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(CapabilityError::Unauthorized),
    }
}

#[derive(Clone)]
pub struct MintedCapability {
    pub(crate) token: CapabilityToken,
    pub(crate) claims: CapabilityClaims,
}

impl MintedCapability {
    pub fn token(&self) -> &CapabilityToken {
        &self.token
    }

    pub fn claims(&self) -> &CapabilityClaims {
        &self.claims
    }

    pub fn id(&self) -> &str {
        &self.claims.capability_id
    }

    pub fn into_bearer_parts(self) -> (String, String, CapabilityClaims) {
        (
            self.token.bearer(),
            self.claims.capability_id.clone(),
            self.claims,
        )
    }
}

pub(crate) fn digest(token: &CapabilityToken) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(token.0).into()
}

pub(crate) fn random_capability_id() -> String {
    let mut bytes = [0_u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    let mut id = String::with_capacity(32);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(id, "{byte:02x}");
    }
    id
}
