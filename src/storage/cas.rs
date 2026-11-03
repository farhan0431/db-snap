//! Content-addressed delta storage (design §3.5).
//!
//! A snapshot is a **manifest**: the ordered list of blake3 hashes of 64 KB
//! page-aligned chunks of the database file. Chunks live in
//! `.dbsnap/objects/<h2>/<hash>` (zstd-compressed) and are shared between
//! snapshots, so a new snapshot only writes chunks that changed.
//!
//! Why fixed page-aligned chunking works so well here: SQLite updates 4 KB
//! pages **in place at fixed offsets** — data never shifts — so identical
//! regions hash identically across snapshots without rolling-hash/CDC tricks.

use anyhow::{bail, Context, Result};
use std::fs::{self, File};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use super::Storage;

/// SQLite's default page size.
pub const PAGE_SIZE: u32 = 4096;
/// 16 pages per chunk → 64 KB.
pub const CHUNK_PAGES: u32 = 16;
pub const CHUNK_SIZE: usize = (PAGE_SIZE * CHUNK_PAGES) as usize;
const MANIFEST_MAGIC: &[u8; 8] = b"DBSNAPM1";
/// Sanity cap so a corrupt manifest cannot make us allocate wildly.
const MAX_CHUNKS: u32 = 16 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Manifest {
    pub page_size: u32,
    pub chunk_pages: u32,
    pub original_size: u64,
    pub hashes: Vec<[u8; 32]>,
}

impl Manifest {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(28 + self.hashes.len() * 32);
        out.extend_from_slice(MANIFEST_MAGIC);
        out.extend_from_slice(&self.page_size.to_le_bytes());
        out.extend_from_slice(&self.chunk_pages.to_le_bytes());
        out.extend_from_slice(&self.original_size.to_le_bytes());
        out.extend_from_slice(&(self.hashes.len() as u32).to_le_bytes());
        for h in &self.hashes {
            out.extend_from_slice(h);
        }
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Manifest> {
        if bytes.len() < 28 || &bytes[0..8] != MANIFEST_MAGIC {
            bail!("not a dbsnap manifest (bad magic)");
        }
        let page_size = u32::from_le_bytes(bytes[8..12].try_into()?);
        let chunk_pages = u32::from_le_bytes(bytes[12..16].try_into()?);
        let original_size = u64::from_le_bytes(bytes[16..24].try_into()?);
        let count = u32::from_le_bytes(bytes[24..28].try_into()?);
        if count > MAX_CHUNKS {
            bail!("manifest chunk count {count} exceeds sanity cap");
        }
        let need = 28 + count as usize * 32;
        if bytes.len() != need {
            bail!(
                "manifest is truncated (expected {need} bytes, got {})",
                bytes.len()
            );
        }
        let hashes = bytes[28..]
            .chunks_exact(32)
            .map(|c| {
                c.try_into()
                    .expect("chunks_exact(32) yields 32-byte slices")
            })
            .collect();
        Ok(Manifest {
            page_size,
            chunk_pages,
            original_size,
            hashes,
        })
    }

    pub fn hexes(&self) -> Vec<String> {
        self.hashes
            .iter()
            .map(|h| blake3::Hash::from_bytes(*h).to_hex().to_string())
            .collect()
    }
}

pub enum DeltaOutcome {
    /// New content — chunks + manifest were written.
    Written {
        manifest_rel: String,
        manifest_size: u64,
        written_bytes: u64,
        content_id: String,
        original_size: u64,
    },
    /// Chunk hashes identical to the previous snapshot — nothing was written.
    Identical { content_id: String },
}

impl Storage {
    pub fn objects_dir(&self) -> PathBuf {
        self.root.join("objects")
    }

    pub fn manifests_dir(&self) -> PathBuf {
        self.root.join("manifests")
    }

    pub fn chunk_path(&self, hex: &str) -> PathBuf {
        self.objects_dir().join(&hex[..2]).join(hex)
    }

    pub fn checked_chunk_path(&self, hex: &str) -> Result<PathBuf> {
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("invalid chunk hash");
        }
        let root = self.objects_dir();
        let dir = root.join(&hex[..2]);
        let path = dir.join(hex);
        for part in [&root, &dir, &path] {
            match fs::symlink_metadata(part) {
                Ok(meta) if meta.file_type().is_symlink() => bail!("chunk path contains a symlink"),
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(path)
    }

    pub fn has_chunk(&self, hex: &str) -> bool {
        self.chunk_path(hex).exists()
    }

    /// Write a chunk (zstd-compressed, atomic rename) unless it already
    /// exists. Returns Some(compressed_len) when actually written.
    pub fn write_chunk_if_missing(&self, hex: &str, data: &[u8]) -> Result<Option<u64>> {
        let path = self.checked_chunk_path(hex)?;
        if path.exists() {
            return Ok(None);
        }
        fs::create_dir_all(path.parent().expect("chunk path has a parent"))?;
        let compressed = zstd::stream::encode_all(data, 3)?;
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, &compressed)?;
        fs::rename(&tmp, &path)?;
        Ok(Some(compressed.len() as u64))
    }

    /// Read + decompress a chunk, verifying its content against its
    /// content-addressed name (per-chunk integrity, design §4-T3).
    pub fn read_chunk_verified(&self, hex: &str) -> Result<Vec<u8>> {
        let path = self.checked_chunk_path(hex)?;
        if !path.exists() {
            bail!(
                "delta object {hex} is missing — the .dbsnap store is incomplete; do not delete .dbsnap/objects manually"
            );
        }
        if fs::metadata(&path)?.len() > (CHUNK_SIZE as u64 * 2) {
            bail!("delta object {hex} exceeds the compressed size limit");
        }
        let compressed = fs::read(&path)?;
        let decoder = zstd::stream::read::Decoder::new(&compressed[..])
            .with_context(|| format!("delta object {hex} is corrupt (zstd decode failed)"))?;
        let mut data = Vec::new();
        decoder.take(CHUNK_SIZE as u64 + 1).read_to_end(&mut data)?;
        if data.len() > CHUNK_SIZE {
            bail!("delta object {hex} exceeds the chunk size limit");
        }
        let actual = blake3::hash(&data).to_hex().to_string();
        if actual != hex {
            bail!(
                "delta object {hex} failed integrity check (got {actual}) — store may be corrupted"
            );
        }
        Ok(data)
    }

    /// Atomically write a manifest for snapshot `id`; returns (rel path, size).
    pub fn write_manifest_file(&self, id: &str, manifest: &Manifest) -> Result<(String, u64)> {
        let rel = format!("manifests/{id}.m");
        let path = self.root.join(&rel);
        let bytes = manifest.to_bytes();
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, &bytes)?;
        fs::rename(&tmp, &path)?;
        Ok((rel, bytes.len() as u64))
    }

    pub fn read_manifest_file(&self, rel: &str) -> Result<Manifest> {
        let bytes = fs::read(self.checked_artifact_path(rel, "manifests")?)
            .with_context(|| format!("reading manifest {rel}"))?;
        Manifest::from_bytes(&bytes).with_context(|| format!("parsing manifest {rel}"))
    }
}

/// Two-pass delta save of the staged (page-consistent) database copy:
/// pass 1 hashes every chunk and computes the content id; pass 2 seeks back
/// and writes only chunks missing from the store.
pub fn save_delta(
    store: &Storage,
    staged: &std::path::Path,
    id: &str,
    prev_content_id: Option<&str>,
) -> Result<DeltaOutcome> {
    let mut file = File::open(staged)?;
    let total = file.metadata()?.len();

    // Pass 1: hash all chunks, remember offsets.
    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut hashes: Vec<[u8; 32]> = Vec::new();
    let mut offsets: Vec<u64> = Vec::new();
    let mut offset = 0u64;
    loop {
        let n = read_full(&mut file, &mut buf)?;
        if n == 0 {
            break;
        }
        hashes.push(*blake3::hash(&buf[..n]).as_bytes());
        offsets.push(offset);
        offset += n as u64;
    }

    let mut content_hasher = blake3::Hasher::new();
    for h in &hashes {
        content_hasher.update(h);
    }
    let content_id = content_hasher.finalize().to_hex().to_string();

    if Some(content_id.as_str()) == prev_content_id {
        for h in &hashes {
            let hex = blake3::Hash::from_bytes(*h).to_hex().to_string();
            store.read_chunk_verified(&hex)?;
        }
        return Ok(DeltaOutcome::Identical { content_id });
    }

    // Pass 2: write only the chunks the store doesn't have yet.
    let mut written_bytes = 0u64;
    for (i, h) in hashes.iter().enumerate() {
        let hex = blake3::Hash::from_bytes(*h).to_hex().to_string();
        if store.checked_chunk_path(&hex)?.exists() {
            store.read_chunk_verified(&hex)?;
            continue;
        }
        let len = std::cmp::min(CHUNK_SIZE as u64, total - offsets[i]) as usize;
        file.seek(SeekFrom::Start(offsets[i]))?;
        read_full(&mut file, &mut buf[..len])?;
        if let Some(compressed) = store.write_chunk_if_missing(&hex, &buf[..len])? {
            written_bytes += compressed;
        }
    }

    let manifest = Manifest {
        page_size: PAGE_SIZE,
        chunk_pages: CHUNK_PAGES,
        original_size: total,
        hashes,
    };
    let (manifest_rel, manifest_size) = store.write_manifest_file(id, &manifest)?;

    Ok(DeltaOutcome::Written {
        manifest_rel,
        manifest_size,
        written_bytes,
        content_id,
        original_size: total,
    })
}

/// Fill `buf` as far as the file allows; returns bytes read (0 = EOF).
fn read_full(file: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match file.read(&mut buf[n..])? {
            0 => break,
            m => n += m,
        }
    }
    Ok(n)
}

/// Reassemble a manifest into a staged database file, verifying every chunk
/// on the way. Returns the staged path and the reconstructed size.
pub fn verify_manifest(manifest: &Manifest, expected_hash: &str, expected_size: u64) -> Result<()> {
    let mut hasher = blake3::Hasher::new();
    for hash in &manifest.hashes {
        hasher.update(hash);
    }
    if hasher.finalize().to_hex().as_str() != expected_hash
        || manifest.original_size != expected_size
        || manifest.page_size != PAGE_SIZE
        || manifest.chunk_pages != CHUNK_PAGES
    {
        bail!("manifest integrity check failed: snapshot metadata does not match manifest");
    }
    Ok(())
}

pub fn stage_from_manifest(
    store: &Storage,
    rel: &str,
    id: &str,
    expected_hash: &str,
    expected_size: u64,
) -> Result<(PathBuf, u64)> {
    let manifest = store.read_manifest_file(rel)?;
    verify_manifest(&manifest, expected_hash, expected_size)?;
    let staged = store.tmp_dir().join(format!("restore-{id}.db"));
    let mut out = BufWriter::with_capacity(1024 * 1024, File::create(&staged)?);
    let mut total = 0u64;
    for hex in manifest.hexes() {
        let data = store.read_chunk_verified(&hex)?;
        out.write_all(&data)?;
        total += data.len() as u64;
    }
    out.flush()?;
    if total != manifest.original_size {
        fs::remove_file(&staged).ok();
        bail!(
            "reconstructed database is {total} bytes but manifest says {} — refusing to restore",
            manifest.original_size
        );
    }
    Ok((staged, total))
}
