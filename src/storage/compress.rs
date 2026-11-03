use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

/// Writer that hashes the bytes flowing through it (of the *compressed*
/// stream — the same stream we verify again on restore).
struct HashWriter<W: Write> {
    inner: W,
    hasher: blake3::Hasher,
}

impl<W: Write> Write for HashWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.hasher.update(buf);
        self.inner.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Compress `src` into `dst` (zstd level 3) while hashing the compressed
/// stream. Returns (original_bytes, compressed_bytes, blake3_hex).
pub fn compress_to(src: &Path, dst: &Path) -> Result<(u64, u64, String)> {
    let reader = BufReader::with_capacity(1024 * 1024, File::open(src)?);
    let out = BufWriter::with_capacity(1024 * 1024, File::create(dst)?);
    let hash_writer = HashWriter {
        inner: out,
        hasher: blake3::Hasher::new(),
    };
    let mut encoder =
        zstd::stream::Encoder::new(hash_writer, 3).context("starting zstd encoder")?;
    std::io::copy(&mut { reader }, &mut encoder).context("compressing snapshot")?;
    let mut hash_writer = encoder.finish().context("finishing zstd stream")?;
    hash_writer.flush()?;
    let original = src.metadata()?.len();
    let compressed = dst.metadata()?.len();
    let hash = hash_writer.hasher.finalize().to_hex().to_string();
    Ok((original, compressed, hash))
}

/// blake3 of a file, streamed (constant memory).
pub fn hash_file(path: &Path) -> Result<String> {
    let reader = BufReader::with_capacity(1024 * 1024, File::open(path)?);
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut { reader }, &mut hasher)?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// Decompress `src` into `dst`, hashing the compressed bytes on the way and
/// comparing against `expected` BEFORE anything else consumes `dst`
/// (verify-before-restore, design §4-T3). Returns the decompressed size.
pub fn decompress_verified(
    src: &Path,
    dst: &Path,
    expected: &str,
    expected_size: u64,
) -> Result<u64> {
    let actual = hash_file(src)?;
    if actual != expected {
        bail!(
            "snapshot integrity check FAILED (hash mismatch)\n  expected blake3: {expected}\n  actual   blake3: {actual}\nRefusing to restore from a corrupt snapshot."
        );
    }
    let result = (|| -> Result<u64> {
        let input = BufReader::with_capacity(1024 * 1024, File::open(src)?);
        let decoder =
            zstd::stream::read::Decoder::new(input).context("opening compressed snapshot")?;
        let mut limited = decoder.take(expected_size.saturating_add(1));
        let mut out = BufWriter::with_capacity(1024 * 1024, File::create(dst)?);
        let written = std::io::copy(&mut limited, &mut out).context("decompressing snapshot")?;
        out.flush()?;
        if written != expected_size {
            bail!("snapshot decompressed size differs from metadata; restore aborted");
        }
        Ok(written)
    })();
    if result.is_err() {
        std::fs::remove_file(dst).ok();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_hash() {
        let dir = std::env::temp_dir().join(format!("dbsnap-zstd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("in.db");
        let zst = dir.join("out.zst");
        let back = dir.join("back.db");
        std::fs::write(&src, vec![7u8; 100_000]).unwrap();

        let (orig, comp, hash) = compress_to(&src, &zst).unwrap();
        assert_eq!(orig, 100_000);
        assert!(comp < 5_000, "repeated bytes should compress well");

        decompress_verified(&zst, &back, &hash, orig).unwrap();
        assert_eq!(std::fs::read(&back).unwrap().len(), 100_000);
        assert!(decompress_verified(&zst, &back, &hash, 10).is_err());
        assert!(
            !back.exists(),
            "failed decompression must remove the staged file"
        );

        // corruption must be detected (either hash mismatch or decode error)
        let mut bytes = std::fs::read(&zst).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xff;
        std::fs::write(&zst, &bytes).unwrap();
        let result = decompress_verified(&zst, &back, &hash, orig);
        assert!(result.is_err(), "corrupt snapshot must fail verification");

        std::fs::remove_dir_all(&dir).ok();
    }
}
