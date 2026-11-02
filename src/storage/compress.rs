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

/// Reader that hashes the compressed bytes passing through it.
struct HashReader<R: Read> {
    inner: R,
    hasher: blake3::Hasher,
}

impl<R: Read> Read for HashReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.hasher.update(&buf[..n]);
        }
        Ok(n)
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
pub fn decompress_verified(src: &Path, dst: &Path, expected: &str) -> Result<u64> {
    let reader = BufReader::with_capacity(1024 * 1024, File::open(src)?);
    let mut hash_reader = HashReader {
        inner: reader,
        hasher: blake3::Hasher::new(),
    };
    let out = BufWriter::with_capacity(1024 * 1024, File::create(dst)?);
    if let Err(e) = zstd::stream::copy_decode(&mut hash_reader, out) {
        bail!("snapshot file is unreadable (corrupt or not a dbsnap snapshot): {e}");
    }
    let actual = hash_reader.hasher.finalize().to_hex().to_string();
    if actual != expected {
        bail!(
            "snapshot integrity check FAILED (hash mismatch)\n  expected blake3: {expected}\n  actual   blake3: {actual}\nRefusing to restore from a corrupt snapshot."
        );
    }
    Ok(dst.metadata()?.len())
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

        decompress_verified(&zst, &back, &hash).unwrap();
        assert_eq!(std::fs::read(&back).unwrap().len(), 100_000);

        // corruption must be detected (either hash mismatch or decode error)
        let mut bytes = std::fs::read(&zst).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xff;
        std::fs::write(&zst, &bytes).unwrap();
        let result = decompress_verified(&zst, &back, &hash);
        assert!(result.is_err(), "corrupt snapshot must fail verification");

        std::fs::remove_dir_all(&dir).ok();
    }
}
