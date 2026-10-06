use crate::config::WasmOptLevel;
use crate::{CliSettings, Result, WasmOptConfig, Workspace};
use anyhow::{Context, bail};
use flate2::read::GzDecoder;
use std::path::{Path, PathBuf};
use tar::Archive;
use tempfile::NamedTempFile;

/// Pinned binaryen version (contains wasm-opt).
const BINARYEN_VERSION: &str = "133-tekacs.1";

/// Write these wasm bytes with a particular set of optimizations
pub async fn write_wasm(bytes: &[u8], output_path: &Path, cfg: &WasmOptConfig) -> Result<()> {
    std::fs::write(output_path, bytes)?;
    optimize(output_path, output_path, cfg).await?;
    Ok(())
}

pub async fn optimize(input_path: &Path, output_path: &Path, cfg: &WasmOptConfig) -> Result<()> {
    let wasm_opt = WasmOpt::new(input_path, output_path, cfg)
        .await
        .context("Failed to create wasm-opt instance")?;
    wasm_opt
        .optimize()
        .await
        .context("Failed to run wasm-opt")?;

    Ok(())
}

struct WasmOpt {
    path: PathBuf,
    input_path: PathBuf,
    temporary_output_path: NamedTempFile,
    output_path: PathBuf,
    cfg: WasmOptConfig,
}

impl WasmOpt {
    pub async fn new(
        input_path: &Path,
        output_path: &Path,
        cfg: &WasmOptConfig,
    ) -> anyhow::Result<Self> {
        let path = get_binary_path().await?;
        Ok(Self {
            path,
            input_path: input_path.to_path_buf(),
            temporary_output_path: tempfile::NamedTempFile::new()?,
            output_path: output_path.to_path_buf(),
            cfg: cfg.clone(),
        })
    }

    /// Create the command to run wasm-opt
    fn build_command(&self) -> tokio::process::Command {
        // defaults needed by wasm-opt.
        // wasm is a moving target, and we add these by default since they progressively get enabled by default.
        let mut args = vec![
            "--enable-reference-types",
            "--enable-bulk-memory",
            "--enable-mutable-globals",
            "--enable-nontrapping-float-to-int",
            "--enable-threads",
        ];

        if self.cfg.memory_packing {
            // needed for our current approach to bundle splitting to work properly
            // todo(jon): emit the main module's data section in chunks instead of all at once
            args.push("--memory-packing");
        }

        if !self.cfg.debug {
            args.push("--strip-debug");
        } else {
            args.push("--debuginfo");
        }

        for extra in &self.cfg.extra_features {
            args.push(extra);
        }

        let level = match self.cfg.level {
            WasmOptLevel::Z => "-Oz",
            WasmOptLevel::S => "-Os",
            WasmOptLevel::Zero => "-O0",
            WasmOptLevel::One => "-O1",
            WasmOptLevel::Two => "-O2",
            WasmOptLevel::Three => "-O3",
            WasmOptLevel::Four => "-O4",
        };

        tracing::debug!(
            "Running wasm-opt: {} {} {} -o {} {}",
            self.path.to_string_lossy(),
            self.input_path.to_string_lossy(),
            level,
            self.temporary_output_path.path().to_string_lossy(),
            args.join(" ")
        );
        let mut command = tokio::process::Command::new(&self.path);
        command
            .arg(&self.input_path)
            .arg(level)
            .arg("-o")
            .arg(self.temporary_output_path.path())
            .args(args);
        command
    }

    pub async fn optimize(&self) -> Result<()> {
        let mut command = self.build_command();
        let res = command.output().await?;

        if !res.status.success() {
            let err = String::from_utf8_lossy(&res.stderr);
            tracing::error!(
                telemetry = %serde_json::json!({ "event": "wasm_opt_failed" }),
                "wasm-opt failed with status code {}\nstderr: {}\nstdout: {}",
                res.status,
                err,
                String::from_utf8_lossy(&res.stdout)
            );
            // A failing wasm-opt execution may leave behind an empty file so copy the original file instead.
            if self.input_path != self.output_path {
                std::fs::copy(&self.input_path, &self.output_path).unwrap();
            }
        } else {
            std::fs::copy(self.temporary_output_path.path(), &self.output_path).unwrap();
        }

        Ok(())
    }
}

fn download_url(os: &str, arch: &str) -> anyhow::Result<String> {
    let platform = match (os, arch) {
        ("windows", "x86_64") => "x86_64-windows",
        ("windows", "aarch64") => "arm64-windows",
        ("linux", "x86_64") => "x86_64-linux",
        ("linux", "aarch64") => "aarch64-linux",
        ("macos", "x86_64") => "x86_64-macos",
        ("macos", "aarch64") => "arm64-macos",
        _ => bail!(
            "Unsupported wasm-opt platform {os}/{arch}. Install wasm-opt manually and enable prefer-no-downloads."
        ),
    };
    Ok(format!(
        "https://github.com/tekacs/binaryen/releases/download/version_{BINARYEN_VERSION}/binaryen-version_{BINARYEN_VERSION}-{platform}.tar.gz"
    ))
}

fn pinned_output(output: &std::process::Output) -> bool {
    output.status.success()
        && std::str::from_utf8(&output.stdout).is_ok_and(|version| {
            version
                .split_whitespace()
                .take(3)
                .eq(["wasm-opt", "version", BINARYEN_VERSION])
        })
}

fn managed_binary(directory: &Path) -> Option<PathBuf> {
    let path = installed_bin_path(directory);
    let output = std::process::Command::new(&path)
        .arg("--version")
        .output()
        .ok()?;
    pinned_output(&output).then_some(path)
}

/// Get the path to the wasm-opt binary, downloading it if necessary.
pub async fn get_binary_path() -> anyhow::Result<PathBuf> {
    if let Some(path) = installed_location() {
        return Ok(path);
    }
    if CliSettings::prefer_no_downloads() {
        bail!("Missing wasm-opt");
    }

    let directory = install_dir();
    let lock_path = directory.with_extension("lock");
    let _lock = tokio::task::spawn_blocking(move || -> anyhow::Result<std::fs::File> {
        std::fs::create_dir_all(lock_path.parent().context("Missing tools directory")?)?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        fs2::FileExt::lock_exclusive(&lock)?;
        Ok(lock)
    })
    .await??;

    // Another installer may have completed while we waited for the lock.
    if let Some(path) = managed_binary(&directory) {
        return Ok(path);
    }
    tracing::info!("Installing wasm-opt {BINARYEN_VERSION}");
    install_github(&directory).await?;
    managed_binary(&directory).context("Installed wasm-opt does not match the pinned version")
}

pub fn installed_location() -> Option<PathBuf> {
    managed_binary(&install_dir()).or_else(|| {
        CliSettings::prefer_no_downloads()
            .then(|| which::which("wasm-opt").ok())
            .flatten()
    })
}

fn install_dir() -> PathBuf {
    Workspace::tools_dir().join(format!("binaryen-{BINARYEN_VERSION}"))
}

fn installed_bin_name() -> &'static str {
    if cfg!(windows) {
        "wasm-opt.exe"
    } else {
        "wasm-opt"
    }
}

fn installed_bin_path(install_dir: &Path) -> PathBuf {
    install_dir.join("bin").join(installed_bin_name())
}

/// Install a verified release while holding the versioned cache lock.
async fn install_github(install_dir: &Path) -> anyhow::Result<()> {
    let url = download_url(std::env::consts::OS, std::env::consts::ARCH)?;
    tracing::trace!("Downloading wasm-opt from {url}");
    let bytes = reqwest::get(url).await?.error_for_status()?.bytes().await?;
    let parent = install_dir.parent().context("Missing tools directory")?;
    let stage = tempfile::tempdir_in(parent)?;
    let bin = installed_bin_path(stage.path());
    let lib = stage.path().join("lib");
    std::fs::create_dir_all(bin.parent().context("Missing binary directory")?)?;
    std::fs::create_dir_all(&lib)?;

    let mut archive = Archive::new(GzDecoder::new(bytes.as_ref()));
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if path
            .file_name()
            .is_some_and(|name| name == installed_bin_name())
        {
            entry.unpack(&bin)?;
        } else if path.components().any(|part| part.as_os_str() == "lib")
            && entry.header().entry_type().is_file()
        {
            if let Some(name) = path.file_name() {
                entry.unpack(lib.join(name))?;
            }
        }
    }
    publish(stage.path(), install_dir)
}

fn publish(stage: &Path, destination: &Path) -> anyhow::Result<()> {
    managed_binary(stage).context("Downloaded wasm-opt does not match the pinned version")?;
    // Only this downloaded version's cache is replaced, and only after verification.
    // A crash in the publication gap leaves a missing cache, which is reinstallable.
    if destination.exists() {
        std::fs::remove_dir_all(destination)?;
    }
    std::fs::rename(stage, destination)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_urls_share_pin() {
        for (os, arch, asset) in [
            ("windows", "x86_64", "x86_64-windows"),
            ("windows", "aarch64", "arm64-windows"),
            ("linux", "x86_64", "x86_64-linux"),
            ("linux", "aarch64", "aarch64-linux"),
            ("macos", "x86_64", "x86_64-macos"),
            ("macos", "aarch64", "arm64-macos"),
        ] {
            assert_eq!(
                download_url(os, arch).unwrap(),
                format!(
                    "https://github.com/tekacs/binaryen/releases/download/version_{BINARYEN_VERSION}/binaryen-version_{BINARYEN_VERSION}-{asset}.tar.gz"
                )
            );
        }
        assert!(download_url("freebsd", "x86_64").is_err());
        assert!(download_url("linux", "riscv64").is_err());
    }

    #[cfg(unix)]
    fn binary(directory: &Path, version: &str, status: i32) {
        use std::os::unix::fs::PermissionsExt;
        let path = installed_bin_path(directory);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' 'wasm-opt version {version} (fixture)'\nexit {status}\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn cache_requires_version() {
        let directory = tempfile::tempdir().unwrap();
        assert!(managed_binary(directory.path()).is_none());
        for (version, status) in [
            ("127", 0),
            ("129", 0),
            ("133", 0),
            ("133-tekacs.10", 0),
            ("", 0),
            (BINARYEN_VERSION, 1),
        ] {
            binary(directory.path(), version, status);
            assert!(managed_binary(directory.path()).is_none());
        }
        binary(directory.path(), BINARYEN_VERSION, 0);
        assert_eq!(
            managed_binary(directory.path()),
            Some(installed_bin_path(directory.path()))
        );
    }

    #[test]
    #[cfg(unix)]
    fn publication_verifies_before_replacing() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory
            .path()
            .join(format!("binaryen-{BINARYEN_VERSION}"));
        let stage = directory.path().join("stage");
        binary(&destination, "127", 0);
        binary(&stage, "127", 0);
        let old = std::fs::read(installed_bin_path(&destination)).unwrap();
        assert!(publish(&stage, &destination).is_err());
        assert_eq!(
            std::fs::read(installed_bin_path(&destination)).unwrap(),
            old
        );
        binary(&stage, BINARYEN_VERSION, 0);
        publish(&stage, &destination).unwrap();
        assert!(managed_binary(&destination).is_some());
        assert!(!stage.exists());
    }
}
