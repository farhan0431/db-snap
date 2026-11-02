use anyhow::{bail, Result};
use chrono::{DateTime, Local, Utc};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

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
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        bail!("tag may only contain letters, digits, '.', '_' and '-' (got: {tag:?})");
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
        assert!(validate_tag("with space").is_err());
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
}
