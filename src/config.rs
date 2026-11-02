use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Sqlite,
    Postgres,
    Mysql,
}

impl std::fmt::Display for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Engine::Sqlite => write!(f, "sqlite"),
            Engine::Postgres => write!(f, "postgres"),
            Engine::Mysql => write!(f, "mysql"),
        }
    }
}

/// Connection target for a network-server database (PostgreSQL or
/// MySQL/MariaDB). Credentials never end up in fingerprints, logs, or
/// metadata (design §4-T11).
#[derive(Debug, Clone)]
pub struct ServerTarget {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: Option<String>,
    pub database: String,
}

impl ServerTarget {
    /// Credential-free identity used in fingerprints and output.
    pub fn display(&self) -> String {
        format!("{}:{}/{}", self.host, self.port, self.database)
    }

    /// libpq-style connstring for spawned clients — no password field; the
    /// password travels via PGPASSWORD env instead (design §4-T10).
    pub fn connstring(&self) -> String {
        format!(
            "host={} port={} user={} dbname={}",
            self.host, self.port, self.user, self.database
        )
    }
}

#[derive(Debug, Clone)]
pub enum DbTarget {
    Sqlite(PathBuf),
    Postgres(ServerTarget),
    Mysql(ServerTarget),
}

/// The database this project is bound to, fully resolved.
#[derive(Debug, Clone)]
pub struct ResolvedDb {
    pub engine: Engine,
    pub target: DbTarget,
}

impl ResolvedDb {
    /// Credential-free, human-readable target description.
    pub fn display(&self) -> String {
        match &self.target {
            DbTarget::Sqlite(p) => p.display().to_string(),
            DbTarget::Postgres(pg) | DbTarget::Mysql(pg) => pg.display(),
        }
    }

    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint {
            engine: self.engine.to_string(),
            target: match &self.target {
                DbTarget::Sqlite(p) => p.display().to_string(),
                DbTarget::Postgres(pg) | DbTarget::Mysql(pg) => pg.display(),
            },
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
/// `allow_remote` is the explicit human-only override (design §4-T1); MCP
/// mode always passes false.
pub fn resolve(explicit: Option<&str>, allow_remote: bool) -> Result<ResolvedDb> {
    if let Some(v) = explicit {
        return parse_db_string(v, allow_remote);
    }
    dotenvy::dotenv().ok();
    if let Ok(v) = std::env::var("DATABASE_URL") {
        let v = v.trim();
        if !v.is_empty() {
            return parse_db_string(v, allow_remote);
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
        target: DbTarget::Sqlite(path),
    })
}

pub fn parse_db_string(raw: &str, allow_remote: bool) -> Result<ResolvedDb> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("database value is empty");
    }

    if let Some((scheme, _rest)) = raw.split_once(':') {
        // "C:\...", "C:/...", "C:file" — Windows drive paths, never schemes
        // (real URL schemes are at least 2 characters).
        if scheme.len() == 1 {
            return sqlite_from(PathBuf::from(raw));
        }
        match scheme {
            "sqlite" | "file" => {
                let rest = raw.split_once(':').unwrap().1;
                let rest = rest.trim_start_matches("//");
                let rest = rest.strip_prefix("localhost").unwrap_or(rest);
                return sqlite_from(PathBuf::from(percent_decode(rest)));
            }
            "postgres" | "postgresql" => return server_from(raw, allow_remote, Engine::Postgres),
            "mysql" | "mariadb" => return server_from(raw, allow_remote, Engine::Mysql),
            _ => bail!(
                "unknown database scheme {scheme:?} in {raw:?} — expected a local path, sqlite://, file://, postgres:// or mysql:// URL"
            ),
        }
    }

    sqlite_from(PathBuf::from(raw))
}

fn server_from(raw: &str, allow_remote: bool, engine: Engine) -> Result<ResolvedDb> {
    let (scheme, default_port, default_user) = match engine {
        Engine::Postgres => ("postgres", 5432, "postgres"),
        Engine::Mysql => ("mysql", 3306, "root"),
        Engine::Sqlite => bail!("sqlite URLs are handled separately"),
    };
    let url = url::Url::parse(raw).with_context(|| format!("parsing {raw:?} as a URL"))?;
    let host = url
        .host_str()
        .with_context(|| format!("{scheme} URL has no host"))?
        .to_string();
    if !is_local_host(&host) && !allow_remote {
        bail!(
            "refusing to touch non-local host {host:?} — dbsnap protects LOCAL databases only\n\
             (guard §4-T1). If you really mean it, pass --allow-remote."
        );
    }
    let port = url.port().unwrap_or(default_port);
    let user = if url.username().is_empty() {
        default_user.to_string()
    } else {
        url.username().to_string()
    };
    let password = url.password().map(percent_decode);
    let database = url.path().trim_start_matches('/').to_string();
    if database.is_empty() {
        bail!("{scheme} URL has no database name: {raw:?}");
    }
    let target = ServerTarget {
        host,
        port,
        user,
        password,
        database,
    };
    Ok(ResolvedDb {
        engine,
        target: match engine {
            Engine::Postgres => DbTarget::Postgres(target),
            Engine::Mysql => DbTarget::Mysql(target),
            Engine::Sqlite => unreachable!("handled above"),
        },
    })
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
        let db = parse_db_string("./prisma/dev.db", false).unwrap();
        assert_eq!(db.engine, Engine::Sqlite);
        let DbTarget::Sqlite(p) = db.target else {
            panic!("expected sqlite target");
        };
        assert!(p.is_absolute());
        assert!(p.ends_with("prisma/dev.db") || p.ends_with("prisma\\dev.db"));
    }

    #[test]
    fn parses_url_forms() {
        for raw in [
            "sqlite:///tmp/x.db",
            "sqlite:/tmp/x.db",
            "file:./x.db",
            "file:///C:/tmp/x.db",
        ] {
            let db = parse_db_string(raw, false).unwrap();
            assert_eq!(db.engine, Engine::Sqlite, "input: {raw}");
            let DbTarget::Sqlite(p) = db.target else {
                panic!("expected sqlite target for {raw}");
            };
            assert!(
                p.to_string_lossy().ends_with("x.db")
                    || p.to_string_lossy().ends_with("tmp\\x.db")
                    || p.to_string_lossy().ends_with("tmp/x.db"),
                "input: {raw} -> {p:?}"
            );
        }
    }

    #[test]
    fn parses_postgres_urls() {
        let db =
            parse_db_string("postgres://dev:secret%20pass@localhost:5433/app_dev", false).unwrap();
        assert_eq!(db.engine, Engine::Postgres);
        let DbTarget::Postgres(ref pg) = db.target else {
            panic!("expected postgres target");
        };
        assert_eq!(pg.host, "localhost");
        assert_eq!(pg.port, 5433);
        assert_eq!(pg.user, "dev");
        assert_eq!(pg.password.as_deref(), Some("secret pass"));
        assert_eq!(pg.database, "app_dev");
        // display & fingerprint never contain the password
        assert!(!pg.display().contains("secret"));
        assert!(!db.fingerprint().target.contains("secret"));
        assert_eq!(
            pg.connstring(),
            "host=localhost port=5433 user=dev dbname=app_dev"
        );
    }

    #[test]
    fn postgres_guard_blocks_remote_hosts() {
        assert!(parse_db_string("postgres://u:p@db.example.com/app", false).is_err());
        // ...but an explicit human override goes through
        assert!(parse_db_string("postgres://u:p@db.example.com/app", true).is_ok());
        assert!(parse_db_string("postgres://u:p@localhost/app", false).is_ok());
        assert!(parse_db_string("postgres://u:p@127.0.0.1/app", false).is_ok());
        // incomplete URL
        assert!(parse_db_string("postgres://localhost", false).is_err());
    }

    #[test]
    fn parses_mysql_urls() {
        let db = parse_db_string("mysql://app:pw@127.0.0.1:3307/app_dev", false).unwrap();
        assert_eq!(db.engine, Engine::Mysql);
        let DbTarget::Mysql(ref m) = db.target else {
            panic!("expected mysql target");
        };
        assert_eq!(m.host, "127.0.0.1");
        assert_eq!(m.port, 3307);
        assert_eq!(m.user, "app");
        assert_eq!(m.database, "app_dev");
        assert!(!db.fingerprint().target.contains("pw"));

        // mariadb:// juga dikenali; default port/user diisi bila absen
        let db = parse_db_string("mariadb://localhost/app", false).unwrap();
        let DbTarget::Mysql(ref m) = db.target else {
            panic!("expected mysql target");
        };
        assert_eq!(m.port, 3306);
        assert_eq!(m.user, "root");
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
