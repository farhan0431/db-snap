use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Sqlite,
}

impl std::fmt::Display for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Engine::Sqlite => write!(f, "sqlite"),
        }
    }
}

/// The database this project is bound to, fully resolved to an absolute path.
#[derive(Debug, Clone)]
pub struct ResolvedDb {
    pub engine: Engine,
    pub path: PathBuf,
}

impl ResolvedDb {
    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint {
            engine: self.engine.to_string(),
            target: self.path.display().to_string(),
        }
    }
}

/// Project binding stored in `.dbsnap/config.json` — engine + canonical target,
/// never credentials (design §4-T11).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Fingerprint {
    pub engine: String,
    pub target: String,
}

/// Resolve which database to operate on: explicit --db flag → DATABASE_URL
/// (from environment or .env) → auto-detection of common local paths.
pub fn resolve(explicit: Option<&str>) -> Result<ResolvedDb> {
    if let Some(v) = explicit {
        return parse_db_string(v);
    }
    dotenvy::dotenv().ok();
    if let Ok(v) = std::env::var("DATABASE_URL") {
        let v = v.trim();
        if !v.is_empty() {
            return parse_db_string(v);
        }
    }
    auto_detect()
}

fn sqlite_from(path: PathBuf) -> Result<ResolvedDb> {
    let abs = if path.is_absolute() {
        path
    } else {
        let cwd = std::env::current_dir().context("cannot determine current directory")?;
        cwd.join(path)
    };
    // canonicalize resolves symlinks; strip the Windows \\?\ prefix so the
    // fingerprint stays comparable.
    let path = std::fs::canonicalize(&abs).unwrap_or(abs);
    let path: PathBuf = std::path::PathBuf::from(
        path.to_string_lossy()
            .strip_prefix(r"\\?\")
            .map(str::to_string)
            .unwrap_or_else(|| path.to_string_lossy().to_string()),
    );
    Ok(ResolvedDb {
        engine: Engine::Sqlite,
        path,
    })
}

pub fn parse_db_string(raw: &str) -> Result<ResolvedDb> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("database value is empty");
    }

    if let Some((scheme, rest)) = raw.split_once(':') {
        // "C:\...", "C:/...", "C:file" — Windows drive paths, never schemes
        // (real URL schemes are at least 2 characters).
        if scheme.len() == 1 {
            return sqlite_from(PathBuf::from(raw));
        }
        match scheme {
            "sqlite" | "file" => {
                let rest = rest.trim_start_matches("//");
                let rest = rest.strip_prefix("localhost").unwrap_or(rest);
                return sqlite_from(PathBuf::from(percent_decode(rest)));
            }
            "postgres" | "postgresql" => bail!(
                "PostgreSQL support arrives in v0.2.0 — dbsnap currently supports local SQLite databases.\nSee docs/DESIGN.md for the multi-engine plan."
            ),
            "mysql" | "mariadb" => bail!(
                "MySQL/MariaDB support arrives in v0.3.0 — dbsnap currently supports local SQLite databases.\nSee docs/DESIGN.md for the multi-engine plan."
            ),
            _ => bail!(
                "unknown database scheme {scheme:?} in {raw:?} — expected a local path, sqlite:// or file:// URL"
            ),
        }
    }

    sqlite_from(PathBuf::from(raw))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Common SQLite locations for local dev projects (checked in order).
const KNOWN_PATHS: &[&str] = &[
    "prisma/dev.db",
    "prisma/dev.sqlite",
    "database/dev.db",
    "data/dev.db",
    "dev.db",
    "dev.sqlite",
    "data.db",
    "database.db",
    "app.db",
];

fn auto_detect() -> Result<ResolvedDb> {
    let cwd = std::env::current_dir().context("cannot determine current directory")?;
    for known in KNOWN_PATHS {
        let p = cwd.join(known);
        if p.is_file() {
            return sqlite_from(p);
        }
    }
    // Fall back to any *.sqlite / *.sqlite3 in the current directory.
    let mut found: Vec<PathBuf> = std::fs::read_dir(&cwd)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .map(|e| e == "sqlite" || e == "sqlite3")
                    .unwrap_or(false)
        })
        .collect();
    found.sort();
    if let Some(p) = found.first() {
        return sqlite_from(p.clone());
    }
    bail!(
        "could not find a SQLite database.\nLooked in {} for {} plus *.sqlite/*.sqlite3.\nSet DATABASE_URL in .env or pass --db <path>.",
        cwd.display(),
        KNOWN_PATHS.join(", ")
    );
}

/// Remote-host guard (design §4-T1). Only local/private targets may be
/// touched without an explicit `--allow-remote` flag. MCP mode never passes
/// that flag, so AI agents are hard-blocked from remote targets.
pub fn is_local_host(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']');
    if h.eq_ignore_ascii_case("localhost") || h.eq_ignore_ascii_case("host.docker.internal") {
        return true;
    }
    if h == "::1" || h == "*" {
        return true;
    }
    if let Some(octets) = h.strip_prefix("127.").map(|s| s.split('.')) {
        return octets.count() >= 1;
    }
    for prefix in ["10.", "192.168."] {
        if h.starts_with(prefix) {
            return true;
        }
    }
    if let Some(rest) = h.strip_prefix("172.") {
        if let Ok(second) = rest.split('.').next().unwrap_or("").parse::<u32>() {
            return (16..=31).contains(&second);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_paths() {
        let db = parse_db_string("./prisma/dev.db").unwrap();
        assert_eq!(db.engine, Engine::Sqlite);
        assert!(db.path.is_absolute());
        assert!(db.path.ends_with("prisma/dev.db") || db.path.ends_with("prisma\\dev.db"));
    }

    #[test]
    fn parses_url_forms() {
        for raw in [
            "sqlite:///tmp/x.db",
            "sqlite:/tmp/x.db",
            "file:./x.db",
            "file:///C:/tmp/x.db",
        ] {
            let db = parse_db_string(raw).unwrap();
            assert_eq!(db.engine, Engine::Sqlite, "input: {raw}");
            assert!(
                db.path.to_string_lossy().ends_with("x.db")
                    || db.path.to_string_lossy().ends_with("tmp\\x.db")
                    || db.path.to_string_lossy().ends_with("tmp/x.db"),
                "input: {raw} -> {:?}",
                db.path
            );
        }
    }

    #[test]
    fn rejects_unsupported_engines() {
        assert!(parse_db_string("postgres://localhost/app").is_err());
        assert!(parse_db_string("mysql://root@127.0.0.1/app").is_err());
    }

    #[test]
    fn local_host_guard() {
        assert!(is_local_host("localhost"));
        assert!(is_local_host("127.0.0.1"));
        assert!(is_local_host("127.0.0.99"));
        assert!(is_local_host("::1"));
        assert!(is_local_host("10.1.2.3"));
        assert!(is_local_host("192.168.0.5"));
        assert!(is_local_host("172.16.0.1"));
        assert!(is_local_host("172.31.255.255"));
        assert!(is_local_host("host.docker.internal"));
        assert!(!is_local_host("db.example.com"));
        assert!(!is_local_host("172.32.0.1"));
        assert!(!is_local_host("8.8.8.8"));
    }
}
