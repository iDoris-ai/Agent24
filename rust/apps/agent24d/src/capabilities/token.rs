use rand::RngCore;

use super::types::{CapabilityClaims, CapabilityError};

/// Opaque 256-bit bearer secret. It has no debug, display, or serialization.
#[derive(Clone, Eq, PartialEq)]
pub struct CapabilityToken(pub(crate) zeroize::Zeroizing<[u8; 32]>);

impl std::hash::Hash for CapabilityToken {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.as_ref().hash(state);
    }
}

impl CapabilityToken {
    pub(crate) fn random() -> Self {
        let mut bytes = zeroize::Zeroizing::new([0_u8; 32]);
        rand::rng().fill_bytes(bytes.as_mut());
        Self(bytes)
    }

    pub(crate) fn bearer(&self) -> String {
        let mut bearer = String::with_capacity(64);
        for byte in self.0.iter() {
            use std::fmt::Write as _;
            let _ = write!(bearer, "{byte:02x}");
        }
        bearer
    }

    pub fn parse_bearer(bearer: &str) -> Result<Self, CapabilityError> {
        if bearer.len() != 64 || !bearer.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(CapabilityError::Unauthorized);
        }
        let mut bytes = zeroize::Zeroizing::new([0_u8; 32]);
        for (index, pair) in bearer.as_bytes().as_chunks::<2>().0.iter().enumerate() {
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
    Sha256::digest(token.0.as_ref()).into()
}

pub(crate) fn random_capability_id() -> String {
    let mut bytes = [0_u8; 16];
    rand::rng().fill_bytes(bytes.as_mut());
    let mut id = String::with_capacity(32);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(id, "{byte:02x}");
    }
    id
}
