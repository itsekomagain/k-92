use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::Error;

/// BLAKE3 hash of an object's content, serialized as 64 lowercase hex characters.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ObjectHash(pub [u8; 32]);

impl ObjectHash {
    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.0 {
            s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
            s.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
        }
        s
    }

    pub fn from_hex(s: &str) -> Result<Self, Error> {
        let bytes = s.as_bytes();
        if bytes.len() != 64 {
            return Err(Error::Corrupt(format!("invalid object hash {s:?}")));
        }
        let mut out = [0u8; 32];
        for (i, pair) in bytes.chunks(2).enumerate() {
            let hi = hex_val(pair[0]);
            let lo = hex_val(pair[1]);
            match (hi, lo) {
                (Some(h), Some(l)) => out[i] = (h << 4) | l,
                _ => return Err(Error::Corrupt(format!("invalid object hash {s:?}"))),
            }
        }
        Ok(ObjectHash(out))
    }
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None, // uppercase rejected: one canonical spelling per hash
    }
}

impl TryFrom<String> for ObjectHash {
    type Error = Error;
    fn try_from(s: String) -> Result<Self, Error> {
        ObjectHash::from_hex(&s)
    }
}

impl From<ObjectHash> for String {
    fn from(h: ObjectHash) -> String {
        h.to_hex()
    }
}

impl fmt::Debug for ObjectHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjectHash({})", &self.to_hex()[..16])
    }
}

impl fmt::Display for ObjectHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip_and_rejects_bad() {
        let h = ObjectHash([0xab; 32]);
        assert_eq!(ObjectHash::from_hex(&h.to_hex()).unwrap(), h);
        assert!(ObjectHash::from_hex("xyz").is_err());
        assert!(ObjectHash::from_hex(&"AB".repeat(32)).is_err());
        assert!(ObjectHash::from_hex(&"../".repeat(21)).is_err());
    }
}
