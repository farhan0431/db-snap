//! Explicit, network-isolated Docker sandbox for trusted MySQL dump snapshots.
use crate::storage::{compress, Location, SnapshotRecord, Storage, TemporaryFile};
use crate::util;
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::process::{Command, Stdio};

pub(super) fn create(store: &Storage, record: &SnapshotRecord, image: &str) -> Result<String> {
    let id = start(store, record, image)?;
    let result = (|| -> Result<()> {
        let registry = store.root.join("sandboxes.json");
        let mut entries: Vec<serde_json::Value> = if registry.exists() {
            serde_json::from_slice(&std::fs::read(&registry)?)?
        } else {
            vec![]
        };
        entries.push(
            json!({"engine":"mysql","container_id":id,"snapshot_id":record.id,"image":image}),
        );
        crate::storage::write_atomic(&registry, &serde_json::to_vec_pretty(&entries)?)?;
        Ok(())
    })();
    cleanup_on_error(&id, result)?;
    Ok(serde_json::to_string_pretty(
        &json!({"operation":"sandbox","engine":"mysql","container_id":id,"image":image,
        "snapshot_id":record.id,"source_modified":false,"network":"none","published_ports":false,
        "usage":"use docker exec with the matching mysql/mariadb client inside this container; no host application endpoint is published",
        "cleanup":format!("docker rm -f -v {id}")}),
    )?)
}

pub(crate) fn start(store: &Storage, record: &SnapshotRecord, image: &str) -> Result<String> {
    let (client, admin) = match image {
        "mysql:8.0" => ("mysql", "mysqladmin"),
        "mariadb:11" => ("mariadb", "mariadb-admin"),
        _ => bail!("sandbox image must be mysql:8.0 or mariadb:11"),
    };
    if record.engine != "mysql"
        || record.method != "mysql-dump"
        || !matches!(record.location, Location::File { .. })
    {
        bail!("Docker sandbox requires a MySQL/MariaDB dump snapshot");
    }
    let token = util::new_id(&store.load_metadata()?.ids());
    let path = store.tmp_dir().join(format!("sandbox-{token}.sql"));
    let _guard = TemporaryFile::create(path.clone())?;
    compress::decompress_verified(
        &store.snapshot_path(record)?,
        &path,
        &record.blake3,
        record.original_bytes,
    )?;
    let owner = owner_label(store)?;
    // No source credentials, published ports, shared network or host mounts.
    // SQL retains its original database identifiers inside this separate server.
    let output = Command::new("docker")
        .args([
            "run",
            "-d",
            "--network",
            "none",
            "--label",
            "dbsnap.sandbox=true",
            "--label",
            &format!("dbsnap.project={owner}"),
            "--name",
            &format!("dbsnap-sandbox-{token}"),
            "-e",
            "MYSQL_ALLOW_EMPTY_PASSWORD=yes",
            "-e",
            "MARIADB_ALLOW_EMPTY_ROOT_PASSWORD=1",
            image,
        ])
        .output()
        .context("Docker is required for --docker-image sandbox")?;
    if !output.status.success() {
        bail!(
            "creating isolated sandbox failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let id = String::from_utf8(output.stdout)?.trim().to_string();
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("Docker returned an unexpected container ID; inspect created sandbox dbsnap-sandbox-{token} manually");
    }
    let result = (|| -> Result<()> {
        let mut ready = false;
        for _ in 0..60 {
            let status = Command::new("docker")
                .args([
                    "exec",
                    &id,
                    admin,
                    "--no-defaults",
                    "ping",
                    "--protocol=TCP",
                    "-h127.0.0.1",
                    "-uroot",
                    "--silent",
                ])
                .output()?;
            if status.status.success() {
                ready = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        if !ready {
            bail!("sandbox server did not become ready within the readiness window");
        }
        let replay = Command::new("docker")
            .args([
                "exec",
                "-i",
                &id,
                client,
                "--no-defaults",
                "--binary-mode",
                "-uroot",
            ])
            .stdin(std::fs::File::open(&path)?)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()?;
        if !replay.status.success() {
            bail!(
                "isolated sandbox replay failed: {}",
                String::from_utf8_lossy(&replay.stderr)
            );
        }
        Ok(())
    })();
    cleanup_on_error(&id, result)?;
    Ok(id)
}

fn owner_label(store: &Storage) -> Result<String> {
    let path = store.root.canonicalize()?;
    // Docker labels contain only a digest. Binding ownership to the canonical
    // project path also prevents a copied registry from deleting resources.
    let identity = serde_json::to_vec(&(path, store.load_config()?))?;
    Ok(blake3::hash(&identity).to_hex().to_string())
}

pub(crate) fn remove_owned(store: &Storage, id: &str) -> Result<()> {
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("sandbox removal requires the exact full container ID");
    }
    let output = Command::new("docker")
        .args(["inspect", "--type", "container", id])
        .output()?;
    if !output.status.success() {
        bail!("unable to inspect the registered sandbox in the current Docker context");
    }
    let entries: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout)?;
    let [entry] = entries.as_slice() else {
        bail!("Docker returned an unexpected number of containers");
    };
    let labels = &entry["Config"]["Labels"];
    if entry["Id"].as_str() != Some(id)
        || labels["dbsnap.sandbox"] != "true"
        || labels["dbsnap.project"].as_str() != Some(owner_label(store)?.as_str())
    {
        bail!("sandbox ownership could not be verified; inspect it and clean it up manually");
    }
    remove(id)
}

fn cleanup_on_error(id: &str, result: Result<()>) -> Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            remove(id)
                .with_context(|| format!("{error:#}; cleanup failed for created sandbox {id}"))?;
            Err(error)
        }
    }
}

pub(crate) fn remove(id: &str) -> Result<()> {
    let output = Command::new("docker")
        .args(["rm", "-f", "-v", id])
        .output()?;
    if !output.status.success() {
        bail!(
            "could not remove isolated sandbox {id}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}
