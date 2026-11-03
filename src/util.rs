use anyhow::{bail, Result};
use chrono::{DateTime, Local, Utc};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Output redaction (design §4-T11) — defense in depth against credentials
// reaching logs, the terminal, or AI agents through MCP tool results.
//
// Layer 1: every password parsed from a URL is registered here; layer 2 is a
// generic `scheme://user:password@` scrubber. Both are applied at every
// output boundary (CLI, MCP results, warnings) regardless of how carefully
// each call site was written.
// ---------------------------------------------------------------------------

fn secrets() -> &'static Mutex<Vec<String>> {
    static SECRETS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    SECRETS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register a secret for redaction. Short strings are ignored — replacing a
/// 2-char fragment would corrupt unrelated output; such secrets are still
/// caught by the URL userinfo scrubber.
pub fn register_secret(secret: &str) {
    if secret.len() < 4 {
        return;
    }
    let mut secrets = secrets().lock().unwrap();
    if !secrets.iter().any(|s| s == secret) {
        secrets.push(secret.to_string());
    }
}

/// Mask every registered secret plus any URL userinfo (`user:password@`)
/// found in `text`. Applied to all human- and agent-readable output.
pub fn mask(text: &str) -> String {
    let registered = secrets().lock().unwrap();
    let mut out = text.to_string();
    for secret in registered.iter() {
        if out.contains(secret.as_str()) {
            out = out.replace(secret.as_str(), "***");
        }
    }
    drop(registered);
    mask_url_userinfo(&out)
}

/// Replace the password part of any `scheme://user:password@host` occurrence
/// with `***` — catches secrets in error chains produced by dependencies,
/// not just the URLs we built ourselves.
fn mask_url_userinfo(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0usize; // everything before this offset is already in `out`
    let mut i = 0usize;
    while i + 3 <= bytes.len() {
        if &bytes[i..i + 3] == b"://" {
            let auth_start = i + 3;
            let auth_end = bytes[auth_start..]
                .iter()
                .position(|b| *b == b'/')
                .map(|p| auth_start + p)
                .unwrap_or(bytes.len());
            let authority = &text[auth_start..auth_end];
            if let Some(at) = authority.rfind('@') {
                let userinfo = &authority[..at];
                if let Some(colon) = userinfo.rfind(':') {
                    out.push_str(&text[copied..auth_start]);
                    out.push_str(&userinfo[..colon]);
                    out.push_str(":***@");
                    copied = auth_start + at + 1;
                }
            }
            // hosts never contain "://" — skip past the authority
            i = auth_end;
        } else {
            i += 1;
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// Short random-looking snapshot id (6 hex chars), collision-checked against
/// the ids already present in metadata.
pub fn new_id(taken: &HashSet<String>) -> String {
    loop {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id() as u64;
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let hash = blake3::hash(format!("{nanos}-{pid}-{counter}").as_bytes());
        let hex = hash.to_hex();
        let id = hex[..6].to_string();
        if !taken.contains(&id) {
            return id;
        }
    }
}

/// Tags end up in metadata (not filenames), but still get validated so agents
/// cannot smuggle anything weird into the catalog.
pub fn validate_tag(tag: &str) -> Result<()> {
    if tag.is_empty() {
        bail!("tag must not be empty");
    }
    if tag.len() > 64 {
        bail!("tag is too long (max 64 bytes)");
    }
    if !tag
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ' '))
    {
        bail!("tag may only contain letters, digits, spaces, '.', '_' and '-' (got: {tag:?})");
    }
    Ok(())
}

pub fn default_tag() -> String {
    Local::now().format("snap-%Y%m%d-%H%M%S").to_string()
}

pub fn safety_tag() -> String {
    Local::now()
        .format("auto-before-restore-%Y%m%d-%H%M%S")
        .to_string()
}

pub fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < MB {
        format!("{:.1} KB", b / KB)
    } else if b < GB {
        format!("{:.1} MB", b / MB)
    } else {
        format!("{:.2} GB", b / GB)
    }
}

pub fn relative_time(created: DateTime<Utc>) -> String {
    let now = Utc::now();
    let secs = (now - created).num_seconds();
    if secs < 0 {
        return created.format("%Y-%m-%d %H:%M").to_string();
    }
    match secs {
        0..=59 => "just now".to_string(),
        60..=119 => "1 min ago".to_string(),
        120..=3599 => format!("{} mins ago", secs / 60),
        3600..=7199 => "1 hour ago".to_string(),
        7200..=86399 => format!("{} hours ago", secs / 3600),
        86400..=172799 => "1 day ago".to_string(),
        _ => created.format("%Y-%m-%d %H:%M").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_validation() {
        assert!(validate_tag("before-migration_2").is_ok());
        assert!(validate_tag("v1.0").is_ok());
        assert!(validate_tag("").is_err());
        assert!(validate_tag("with space").is_ok());
        assert!(validate_tag("../escape").is_err());
        assert!(validate_tag(&"x".repeat(65)).is_err());
    }

    #[test]
    fn ids_are_unique_against_taken() {
        let mut taken = HashSet::new();
        let a = new_id(&taken);
        taken.insert(a.clone());
        let b = new_id(&taken);
        assert_ne!(a, b);
        assert_eq!(a.len(), 6);
    }

    #[test]
    fn sizes_are_human() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(2048), "2.0 KB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn masks_registered_secrets() {
        register_secret("super-secret-pw");
        let out = mask("connect failed for super-secret-pw at db");
        assert_eq!(out, "connect failed for *** at db");
        // tidak mengubah teks tanpa secret
        assert_eq!(mask("nothing here"), "nothing here");
    }

    #[test]
    fn masks_url_userinfo_everywhere() {
        let text = "died connecting to postgres://dev:hunter2@db.local:5432/app, \
                    retrying mysql://root:s3cret@127.0.0.1/db, and plain mysql://anon@host/db";
        let out = mask(text);
        assert!(!out.contains("hunter2"), "{out}");
        assert!(!out.contains("s3cret"), "{out}");
        assert!(
            out.contains("postgres://dev:***@db.local:5432/app"),
            "{out}"
        );
        assert!(out.contains("mysql://root:***@127.0.0.1/db"), "{out}");
        // userinfo tanpa password tetap utuh
        assert!(out.contains("mysql://anon@host/db"), "{out}");
    }

    #[test]
    fn masking_is_unicode_safe() {
        let out = mask("utf8 jalan: postgres://u:pw@h/db — あändern ✓");
        assert!(out.contains("あändern ✓"), "{out}");
        assert!(out.contains("u:***@h"), "{out}");
    }
}
