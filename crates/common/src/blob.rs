//! Content-addressed evidence helpers: sha256 keys and zstd compression.

use std::io::Read;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

const ZSTD_LEVEL: i32 = 3;

/// sha256 of the uncompressed bytes (the `blobs.sha256` key).
pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// Compresses into a zstd frame at level 3.
pub fn compress(data: &[u8]) -> Vec<u8> {
    // Encoding into an in-memory Vec cannot fail short of allocation failure.
    zstd::bulk::compress(data, ZSTD_LEVEL).expect("zstd compression into memory")
}

/// Decompresses a zstd frame, refusing to produce more than `max_len` bytes.
pub fn decompress(data: &[u8], max_len: usize) -> Result<Vec<u8>> {
    let decoder = zstd::stream::read::Decoder::new(data).context("open zstd frame")?;
    // Read one byte past the limit so an oversized frame is detected, not truncated.
    let mut limited = decoder.take(max_len as u64 + 1);
    let mut out = Vec::new();
    limited
        .read_to_end(&mut out)
        .context("decompress zstd frame")?;
    if out.len() > max_len {
        bail!("decompressed blob exceeds {max_len} bytes");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_restores_bytes() {
        let data: Vec<u8> = (0..50_000u32)
            .flat_map(|i| (i % 251).to_le_bytes())
            .collect();
        let packed = compress(&data);
        assert!(packed.len() < data.len());
        assert_eq!(decompress(&packed, data.len()).unwrap(), data);
        assert_eq!(decompress(&compress(b""), 0).unwrap(), b"");
    }

    #[test]
    fn max_len_guard_refuses_inflation() {
        let data = vec![b'a'; 10_000];
        let packed = compress(&data);
        assert!(decompress(&packed, 9_999).is_err());
        assert_eq!(decompress(&packed, 10_000).unwrap().len(), 10_000);
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}
