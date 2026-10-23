//! Bind observations to the deployed bytes, not the checkout at test time.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use super::{Checks, field, host_exe, pretty, read_json_file, validator_exe, winit_exe};
use crate::{Result, ResultExt};

fn git(root: &Path, arguments: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Err(format!("git {} failed", arguments.join(" ")).into());
    }
    Ok(output.stdout)
}

fn digest(bytes: &[u8]) -> Result<String> {
    let mut child = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let written = child.stdin.take().ok_or("no hash input")?.write_all(bytes);
    let output = child.wait_with_output()?;
    written?;
    if !output.status.success() {
        return Err("sha256sum failed".into());
    }
    let hash = String::from_utf8(output.stdout).context("hash output")?;
    Ok(hash
        .split_whitespace()
        .next()
        .ok_or("no SHA-256")?
        .to_owned())
}

pub(super) fn source(root: &Path) -> Result<Value> {
    let head = String::from_utf8(git(root, &["rev-parse", "HEAD"])?)
        .context("Git HEAD")?
        .trim()
        .to_owned();
    let status = String::from_utf8(git(
        root,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?)
    .context("Git status")?;
    let files = git(
        root,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )?;
    let mut paths: Vec<&[u8]> = files
        .split(|&byte| byte == 0)
        .filter(|path| !path.is_empty())
        .collect();
    paths.sort_unstable();
    paths.dedup();
    let mut inventory = Vec::new();
    for name in paths {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::PermissionsExt;
        let path = root.join(std::ffi::OsStr::from_bytes(name));
        inventory.extend_from_slice(name);
        inventory.push(0);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                inventory.extend_from_slice(&metadata.permissions().mode().to_le_bytes());
                let bytes = if metadata.file_type().is_symlink() {
                    fs::read_link(&path)?.as_os_str().as_bytes().to_vec()
                } else {
                    fs::read(&path).context(path.display())?
                };
                inventory.extend_from_slice(digest(&bytes)?.as_bytes());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                inventory.extend_from_slice(b"deleted")
            }
            Err(error) => return Err(error.into()),
        }
        inventory.push(0);
    }
    let sha256 = digest(&inventory)?;
    let dirty = !status.is_empty();
    let revision = if dirty {
        format!("{head}-dirty-{}", &sha256[..12])
    } else {
        head.clone()
    };
    Ok(
        json!({"head": head, "dirty": dirty, "revision": revision, "source_sha256": sha256, "status": status}),
    )
}

pub(super) fn manifest_path() -> PathBuf {
    crate::repo_root().join("target/windows-vm-build.json")
}

fn artifacts() -> Result<Value> {
    let mut hashes = Map::new();
    for path in [host_exe(), winit_exe(), validator_exe()] {
        let name = path
            .file_name()
            .ok_or("no artifact name")?
            .to_string_lossy()
            .into_owned();
        hashes.insert(
            name,
            digest(&fs::read(&path).context(path.display())?)?.into(),
        );
    }
    Ok(hashes.into())
}

pub(super) fn finish_build(before: Value) -> Result<()> {
    if source(&crate::repo_root())? != before {
        return Err("source changed during the build; rerun `cargo winvm-checks build` to obtain reliable provenance".into());
    }
    let manifest = json!({"schema": 1, "source": before, "artifacts": artifacts()?,
        "built_unix_seconds": SystemTime::now().duration_since(UNIX_EPOCH).context("build time")?.as_secs()});
    fs::write(manifest_path(), pretty(&manifest)?)?;
    println!("Build revision: {}", manifest["source"]["revision"]);
    Ok(())
}

pub(super) fn deployment() -> Result<PathBuf> {
    let mut manifest = read_json_file(&manifest_path()).map_err(|error| {
        format!("build provenance unavailable; run `cargo winvm-checks build`: {error}")
    })?;
    if manifest["artifacts"] != artifacts()? {
        return Err(
            "build artifacts changed since their manifest was recorded; rebuild before deploying"
                .into(),
        );
    }
    manifest["guest_helper_sha256"] = digest(&fs::read(
        crate::repo_root().join("scripts/windows-vm/guest/qa.ps1"),
    )?)?
    .into();
    let path = crate::repo_root().join("target/windows-vm-deployment.json");
    fs::write(&path, pretty(&manifest)?)?;
    Ok(path)
}

pub(super) fn deployed(checks: &Checks) -> Result<Value> {
    let manifest = checks.guest(r#"$ErrorActionPreference = 'Stop'
$manifest = Get-Content -Raw -Encoding UTF8 C:\textloom-qa\deployment.json | ConvertFrom-Json
$paths = @{
    'textloom-native-example.exe' = 'C:\textloom-qa\host\textloom-native-example.exe'
    'tlqa-winit-harness.exe' = 'C:\textloom-qa\winit\tlqa-winit-harness.exe'
    'check-native-report.exe' = 'C:\textloom-qa\check-native-report.exe'
}
foreach ($name in $paths.Keys) {
    if ((Get-FileHash -Algorithm SHA256 $paths[$name]).Hash -ne $manifest.artifacts.$name) { throw "deployed artifact hash mismatch: $name" }
}
if ((Get-FileHash -Algorithm SHA256 C:\textloom-qa\qa.ps1).Hash -ne $manifest.guest_helper_sha256) { throw 'deployed guest helper hash mismatch' }
Out-Json $manifest"#)?;
    if manifest["schema"] != 1 || !manifest["source"]["revision"].is_string() {
        return Err("deployed build provenance has an invalid schema or revision".into());
    }
    Ok(manifest)
}

pub(super) fn revision(checks: &Checks) -> Result<String> {
    field(&deployed(checks)?["source"], "revision")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_tracks_untracked_edits_and_deletions() {
        let directory = crate::TempDir::try_new("winvm-provenance-").unwrap();
        let root = directory.path();
        git(root, &["init", "--quiet"]).unwrap();
        fs::write(root.join("tracked"), "original").unwrap();
        git(root, &["add", "."]).unwrap();
        git(
            root,
            &[
                "-c",
                "user.name=QA",
                "-c",
                "user.email=qa@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ],
        )
        .unwrap();
        let clean = source(root).unwrap();
        assert_eq!(clean["dirty"], false);
        assert_eq!(clean["revision"], clean["head"]);
        fs::write(root.join("new"), "one").unwrap();
        let dirty = source(root).unwrap();
        assert_eq!(dirty["dirty"], true);
        assert!(dirty["revision"].as_str().unwrap().contains("-dirty-"));
        fs::write(root.join("new"), "two").unwrap();
        assert_ne!(source(root).unwrap()["revision"], dirty["revision"]);
        fs::remove_file(root.join("new")).unwrap();
        assert_eq!(source(root).unwrap(), clean);
        fs::remove_file(root.join("tracked")).unwrap();
        assert_ne!(
            source(root).unwrap()["source_sha256"],
            clean["source_sha256"]
        );
    }
}
