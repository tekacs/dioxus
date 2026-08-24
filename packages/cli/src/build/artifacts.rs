use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    fs::{File, OpenOptions},
    io,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    time::SystemTime,
};

const BUILD_LOCK: &str = ".cargo-build-lock";
const TARGET_LOCK: &str = ".cargo-artifact-lock";
const LEGACY_LOCK: &str = ".cargo-lock";
const LOCKS: &[&str] = &[BUILD_LOCK, TARGET_LOCK, LEGACY_LOCK];
const CARGO_DIRS: &[&str] = &[".fingerprint", "build", "deps", "examples", "incremental"];

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Marker {
    build: PathBuf,
    target: Option<PathBuf>,
}

struct Locks {
    _files: Vec<File>,
}

/// A durable marker around one cancellable dx build generation.
///
/// Ordinary completion removes the marker. Task cancellation or process death
/// leaves it behind; the next generation acquires Cargo's build/artifact locks
/// and invalidates every unit or output touched since the marker was published.
pub(super) struct Artifacts {
    path: PathBuf,
    marker: Marker,
}

impl Artifacts {
    pub(super) fn start(path: PathBuf, build: PathBuf, target: PathBuf) -> Result<Self> {
        let target = (target != build).then_some(target);
        let artifacts = Self {
            path,
            marker: Marker { build, target },
        };
        artifacts.recover()?;
        artifacts.publish()?;
        Ok(artifacts)
    }

    pub(super) fn finish(self) -> Result<()> {
        remove_file(&self.path)
    }

    fn recover(&self) -> Result<()> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| format!("read {}", self.path.display()));
            }
        };
        let since = fs::metadata(&self.path)
            .and_then(|metadata| metadata.modified())
            .with_context(|| format!("read marker time from {}", self.path.display()))?;
        let marker: Marker = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse {}", self.path.display()))?;
        let _locks = Locks::acquire(&marker)?;
        scrub(&marker.build, since)?;
        if let Some(target) = &marker.target {
            scrub(target, since)?;
        }
        remove_file(&self.path)
    }

    fn publish(&self) -> Result<()> {
        let parent = self
            .path
            .parent()
            .context("dx artifact marker has no parent")?;
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        let temporary = self
            .path
            .with_extension(format!("json.{}.tmp", std::process::id()));
        let bytes = serde_json::to_vec(&self.marker).context("serialize dx artifact marker")?;
        let mut file =
            File::create(&temporary).with_context(|| format!("create {}", temporary.display()))?;
        std::io::Write::write_all(&mut file, &bytes)
            .and_then(|()| std::io::Write::write_all(&mut file, b"\n"))
            .and_then(|()| file.sync_all())
            .with_context(|| format!("write {}", temporary.display()))?;
        fs::rename(&temporary, &self.path).with_context(|| {
            format!(
                "replace {} with {}",
                self.path.display(),
                temporary.display()
            )
        })?;
        Ok(())
    }
}

impl Locks {
    fn acquire(marker: &Marker) -> Result<Self> {
        let mut files = vec![lock_dir(&marker.build, BUILD_LOCK)?];
        if let Some(target) = &marker.target {
            files.push(lock_dir(target, TARGET_LOCK)?);
        }
        Ok(Self { _files: files })
    }
}

fn lock_dir(dir: &Path, name: &str) -> Result<File> {
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join(name);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("open Cargo lock {}", path.display()))?;
    flock(&file).with_context(|| format!("lock Cargo directory {}", dir.display()))?;
    Ok(file)
}

fn scrub(dir: &Path, since: SystemTime) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    scrub_groups(&dir.join("deps"), since)?;
    scrub_groups(&dir.join("examples"), since)?;
    for name in ["build", ".fingerprint", "incremental"] {
        scrub_units(&dir.join(name), since)?;
    }
    scrub_top(dir, since)
}

fn scrub_groups(dir: &Path, since: SystemTime) -> Result<()> {
    let entries = read_dir(dir)?;
    let mut files = Vec::new();
    let mut touched = BTreeSet::new();
    for entry in entries {
        let path = entry.path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).with_context(|| format!("stat {}", path.display())),
        };
        if metadata.is_dir() {
            scrub_groups(&path, since)?;
            continue;
        }
        let Some(key) = group_key(&entry.file_name()) else {
            continue;
        };
        if modified(&metadata, since) {
            touched.insert(key.clone());
        }
        files.push((path, key));
    }
    for (path, key) in files {
        if touched.contains(&key) {
            remove_file(&path)?;
        }
    }
    Ok(())
}

fn scrub_units(dir: &Path, since: SystemTime) -> Result<()> {
    for entry in read_dir(dir)? {
        let path = entry.path();
        if tree_touched(&path, since)? {
            remove(&path)?;
        }
    }
    Ok(())
}

fn scrub_top(dir: &Path, since: SystemTime) -> Result<()> {
    for entry in read_dir(dir)? {
        let name = entry.file_name();
        if LOCKS.iter().any(|lock| name == *lock) || CARGO_DIRS.iter().any(|known| name == *known) {
            continue;
        }
        let path = entry.path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).with_context(|| format!("stat {}", path.display())),
        };
        if !metadata.is_dir() && modified(&metadata, since) {
            remove_file(&path)?;
        }
    }
    Ok(())
}

fn group_key(name: &std::ffi::OsStr) -> Option<String> {
    let name = name.to_str()?;
    let (base, suffix) = name.split_once('.').unwrap_or((name, ""));
    let archive = matches!(
        suffix.split('.').next().unwrap_or(""),
        "a" | "dylib" | "rlib" | "rmeta" | "so"
    );
    Some(
        if archive {
            base.strip_prefix("lib").unwrap_or(base)
        } else {
            base
        }
        .to_owned(),
    )
}

fn tree_touched(path: &Path, since: SystemTime) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("stat {}", path.display())),
    };
    if modified(&metadata, since) {
        return Ok(true);
    }
    if !metadata.is_dir() {
        return Ok(false);
    }
    for entry in read_dir(path)? {
        if tree_touched(&entry.path(), since)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn modified(metadata: &fs::Metadata, since: SystemTime) -> bool {
    metadata
        .modified()
        .map(|time| time >= since)
        .unwrap_or(false)
}

fn read_dir(path: &Path) -> Result<Vec<fs::DirEntry>> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    entries
        .map(|entry| entry.with_context(|| format!("read entry in {}", path.display())))
        .collect()
}

fn remove(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("stat {}", path.display())),
    };
    if metadata.is_dir() {
        match fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
        }
    } else {
        remove_file(path)
    }
}

fn remove_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

fn flock(file: &File) -> Result<()> {
    loop {
        // SAFETY: `file` owns a valid descriptor for this call's duration.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error).context("acquire Cargo lock");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{thread, time::Duration};

    fn put(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn interrupted_generation_invalidates_touched_groups_and_units() {
        let root = tempfile::tempdir().unwrap();
        let build = root.path().join("build");
        let target = root.path().join("target");
        let marker = root.path().join("marker.json");
        let keep = build.join("deps/libkeep-123.rlib");
        put(&keep, b"complete");
        thread::sleep(Duration::from_millis(20));
        let artifacts = Artifacts::start(marker.clone(), build.clone(), target.clone()).unwrap();
        let archive = build.join("deps/libchanged-456.rlib");
        let metadata = build.join("deps/libchanged-456.rmeta");
        let incremental = build.join("incremental/changed/s-1/dep-graph.bin");
        let fingerprint = build.join(".fingerprint/changed-456/invoked.timestamp");
        let output = target.join("render.wasm");
        for path in [&archive, &metadata, &incremental, &fingerprint, &output] {
            put(path, b"stale");
        }
        drop(artifacts);

        let recovered = Artifacts::start(marker, build.clone(), target).unwrap();
        assert!(keep.exists());
        for path in [archive, metadata, incremental, fingerprint, output] {
            assert!(!path.exists(), "{} survived recovery", path.display());
        }
        recovered.finish().unwrap();
    }

    #[test]
    fn ordinary_settlement_preserves_outputs() {
        let root = tempfile::tempdir().unwrap();
        let build = root.path().join("build");
        let target = root.path().join("target");
        let marker = root.path().join("marker.json");
        let artifacts = Artifacts::start(marker.clone(), build, target.clone()).unwrap();
        let output = target.join("render.wasm");
        put(&output, b"complete");
        artifacts.finish().unwrap();
        assert!(output.exists());
        assert!(!marker.exists());
    }
}
