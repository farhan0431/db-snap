//! Find native database clients even when a GUI launches MCP with a short PATH.
use std::path::PathBuf;

pub(super) fn candidates(bin: &str, packages: &[&str]) -> Vec<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<_> = std::env::split_paths(&path).collect();
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let mut dirs = dirs;
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    for prefix in ["/opt/homebrew", "/usr/local", "/home/linuxbrew/.linuxbrew"] {
        for package in packages {
            dirs.push(PathBuf::from(prefix).join("opt").join(package).join("bin"));
        }
        dirs.push(PathBuf::from(prefix).join("bin"));
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = packages;

    collect_candidates(bin, dirs)
}

fn collect_candidates(bin: &str, dirs: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let executable = if cfg!(windows) {
        format!("{bin}.exe")
    } else {
        bin.to_owned()
    };
    let mut result = Vec::new();
    for dir in dirs {
        // Preserve explicit relative PATH entries but do not implicitly search
        // the working directory when launching a native database client.
        if dir.as_os_str().is_empty() {
            continue;
        }
        let path = dir.join(&executable);
        let Ok(metadata) = path.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                continue;
            }
        }
        // Keep the PATH symlink spelling: wrappers may depend on argv[0].
        let Ok(path) = std::path::absolute(path) else {
            continue;
        };
        if !result.contains(&path) {
            result.push(path);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::TemporaryFile;

    #[test]
    fn client_paths_keep_precedence_and_skip_missing_entries() -> anyhow::Result<()> {
        let root = std::env::temp_dir().join(format!("dbsnap-clients-{}", std::process::id()));
        let first = root.join("first");
        let fallback = root.join("fallback");
        std::fs::create_dir_all(&first)?;
        std::fs::create_dir_all(&fallback)?;
        let executable = if cfg!(windows) {
            "dbsnap-test-client.exe"
        } else {
            "dbsnap-test-client"
        };
        let paths = [first.join(executable), fallback.join(executable)];
        for path in &paths {
            let guard = TemporaryFile::create(path.clone())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
            }
            guard.persist();
        }
        let actual = collect_candidates(
            "dbsnap-test-client",
            [root.join("missing"), first.clone(), first, fallback],
        );
        for path in &paths {
            std::fs::remove_file(path)?;
        }
        assert_eq!(actual, paths);
        Ok(())
    }
}
