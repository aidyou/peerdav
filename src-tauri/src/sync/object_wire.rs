use super::repository_types::{WireError, OBJECT_HEADER_LIMIT, OBJECT_WIRE_MAGIC};
use serde::{Deserialize, Serialize};

/// A bounded canonical header followed by raw authenticated bytes. No payload
/// length from untrusted input is used to allocate memory.
pub fn encode<T: Serialize>(header: &T, payload: &[u8], limit: usize) -> Result<Vec<u8>, WireError> {
    let header = super::repository_types::encode(header, OBJECT_HEADER_LIMIT)?;
    let size = 12usize.checked_add(header.len()).and_then(|size| size.checked_add(payload.len()))
        .filter(|size| *size <= limit).ok_or(WireError::TooLarge)?;
    let mut bytes = Vec::with_capacity(size);
    bytes.extend_from_slice(OBJECT_WIRE_MAGIC);
    bytes.extend_from_slice(&(header.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&header);
    bytes.extend_from_slice(payload);
    Ok(bytes)
}

pub fn decode<T>(bytes: &[u8], limit: usize) -> Result<(T, &[u8]), WireError>
where T: for<'de> Deserialize<'de> + Serialize {
    if bytes.len() > limit { return Err(WireError::TooLarge); }
    if bytes.get(..8) != Some(OBJECT_WIRE_MAGIC.as_slice()) { return Err(WireError::Invalid("object magic")); }
    let length: [u8; 4] = bytes.get(8..12).ok_or(WireError::Invalid("object header"))?
        .try_into().map_err(|_| WireError::Invalid("object header"))?;
    let length = u32::from_be_bytes(length) as usize;
    if length > OBJECT_HEADER_LIMIT { return Err(WireError::TooLarge); }
    let end = 12usize.checked_add(length).ok_or(WireError::TooLarge)?;
    let header = bytes.get(12..end).ok_or(WireError::Invalid("truncated object header"))?;
    let value = super::repository_types::decode(header, OBJECT_HEADER_LIMIT)?;
    Ok((value, &bytes[end..]))
}

impl super::repository_types::Envelope {
    pub fn to_wire(&self) -> Result<Vec<u8>, WireError> {
        encode(self, &self.ciphertext, super::repository_types::MAX_METADATA_SIZE + super::repository_types::OBJECT_WIRE_OVERHEAD)
    }

    pub fn from_wire(bytes: &[u8]) -> Result<Self, WireError> {
        let (mut header, payload): (Self, _) = decode(bytes, super::repository_types::MAX_METADATA_SIZE + super::repository_types::OBJECT_WIRE_OVERHEAD)?;
        let limit = match header.kind.as_str() {
            "block" => super::repository_types::MAX_BLOCK_SIZE,
            "tree" | "commit" => super::repository_types::MAX_METADATA_SIZE,
            _ => return Err(WireError::Invalid("object kind")),
        };
        if payload.len() < 16 || payload.len() > limit + 16 { return Err(WireError::Invalid("ciphertext length")); }
        header.ciphertext = payload.to_vec();
        Ok(header)
    }
}

impl super::append_types::Envelope {
    pub fn to_wire(&self) -> Result<Vec<u8>, WireError> {
        encode(self, &self.payload, super::append_types::MAX_METADATA_SIZE + super::repository_types::OBJECT_WIRE_OVERHEAD)
    }

    pub fn from_wire(bytes: &[u8]) -> Result<Self, WireError> {
        let (mut header, payload): (Self, _) = decode(bytes, super::append_types::MAX_METADATA_SIZE + super::repository_types::OBJECT_WIRE_OVERHEAD)?;
        let limit = match header.kind.as_str() {
            "block" => super::append_types::MAX_BLOCK_SIZE,
            "tree" | "commit" => super::append_types::MAX_METADATA_SIZE,
            _ => return Err(WireError::Invalid("object kind")),
        };
        let tag = match header.mode { super::append_types::AppendMode::Encrypted => 16, super::append_types::AppendMode::AuthenticatedPlain => 0 };
        let length = usize::try_from(header.plaintext_len).map_err(|_| WireError::TooLarge)?;
        if length > limit || length.checked_add(tag) != Some(payload.len()) { return Err(WireError::Invalid("payload length")); }
        header.payload = payload.to_vec();
        Ok(header)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_payload_and_exact_limits_reject_truncation_and_noncanonical_headers() {
        let value = vec![1u64, 2];
        let payload = b"\0\xffbinary\n";
        let bytes = encode(&value, payload, 100).expect("wire");
        assert_eq!(&bytes[12..17], b"[1,2]");
        assert_eq!(&bytes[17..], payload);
        let (decoded, raw): (Vec<u64>, _) = decode(&bytes, bytes.len()).expect("decode");
        assert_eq!(decoded, value); assert_eq!(raw, payload);
        assert!(encode(&value, payload, bytes.len() - 1).is_err());
        assert!(decode::<Vec<u64>>(&bytes, bytes.len() - 1).is_err());
        for end in 0..17 { assert!(decode::<Vec<u64>>(&bytes[..end], 100).is_err()); }
        let mut bad = bytes.clone(); bad[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(decode::<Vec<u64>>(&bad, 100).is_err());
        let mut bad = bytes.clone(); bad[0] ^= 1;
        assert!(decode::<Vec<u64>>(&bad, 100).is_err());
        let mut bad = bytes.clone(); bad[12..17].copy_from_slice(b"[1, ]");
        assert!(decode::<Vec<u64>>(&bad, 100).is_err());
    }
}
