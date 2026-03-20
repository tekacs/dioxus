use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    env::{args, vars},
    path::PathBuf,
    process::ExitCode,
};

/// A "capture" of a workspace's rustc commands, cumulated by reading the various rustc commands
/// from disk.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceRustcArgs {
    pub link_args: Vec<String>,
    pub rustc_args: HashMap<String, RustcArgs>,
}

impl WorkspaceRustcArgs {
    pub fn new(link_args: Vec<String>) -> Self {
        Self {
            link_args,
            rustc_args: Default::default(),
        }
    }
}

#[derive(Default, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RustcArgs {
    pub args: Vec<String>,
    pub envs: Vec<(String, String)>,
    #[serde(default)]
    pub cwd: PathBuf,
    /// it doesn't include first program name argument
    pub link_args: Vec<String>,
}

impl RustcArgs {
    pub fn replay(&self) -> std::process::Command {
        let rustc = self.args.first().map(String::as_str).unwrap_or("rustc");
        let mut cmd = std::process::Command::new(rustc);
        cmd.args(self.args.iter().skip(1));
        cmd.env_clear();
        cmd.envs(self.envs.iter().cloned());
        if !self.cwd.as_os_str().is_empty() {
            cmd.current_dir(&self.cwd);
        }
        cmd
    }
}

/// The environment variable indicating where the args directory is located.
///
/// When `dx-rustc` runs, it writes each workspace crate's arguments to a
/// separate file in this directory: `{dir}/{crate_name}.json`.
pub const DX_RUSTC_WRAPPER_ENV_VAR: &str = "DX_RUSTC";

/// Is `dx` being used as a rustc wrapper?
///
/// This is primarily used to intercept cargo, enabling fast hot-patching by caching the environment
/// cargo setups up for the user's current project.
///
/// In a different world we could simply rely on cargo printing link args and the rustc command, but
/// it doesn't seem to output that in a reliable, parseable, cross-platform format (ie using command
/// files on windows...), so we're forced to do this interception nonsense.
pub fn is_wrapping_rustc() -> bool {
    std::env::var(DX_RUSTC_WRAPPER_ENV_VAR).is_ok()
}

/// Check if the arguments indicate a linking step, including those in command files.
fn has_linking_args() -> bool {
    for arg in std::env::args() {
        if arg.ends_with(".o") || arg == "-flavor" {
            return true;
        }

        if let Some(path_str) = arg.strip_prefix('@') {
            if let Ok(file_binary) = std::fs::read(path_str) {
                let content = String::from_utf8(file_binary.clone()).unwrap_or_else(|_| {
                    let binary_u16le: Vec<u16> = file_binary
                        .chunks_exact(2)
                        .map(|a| u16::from_le_bytes([a[0], a[1]]))
                        .collect();
                    String::from_utf16_lossy(&binary_u16le)
                });

                if content.lines().any(|line| {
                    let trimmed_line = line.trim().trim_matches('"');
                    trimmed_line.ends_with(".o") || trimmed_line == "-flavor"
                }) {
                    return true;
                }
            }
        }
    }

    false
}

/// Run rustc directly, but output the result to a per-crate file in the args directory.
///
/// <https://doc.rust-lang.org/cargo/reference/config.html#buildrustc>
pub fn run_rustc() -> ExitCode {
    let args_dir: PathBuf = std::env::var(DX_RUSTC_WRAPPER_ENV_VAR)
        .expect("DX_RUSTC env var must be set")
        .into();

    // Cargo invokes a workspace wrapper like: `wrapper-name rustc [args...]`.
    // We skip our own executable name (`wrapper-name`) to get the args passed to us.
    let captured_args = args().skip(1).collect::<Vec<_>>();

    let rustc_args = RustcArgs {
        args: captured_args,
        envs: vars().collect::<_>(),
        cwd: std::env::current_dir().expect("Failed to get current dir"),
        link_args: Default::default(),
    };

    // Always persist the captured rustc invocation, even for link steps.
    // The tip crate's bin target is typically only observed during the final link invocation,
    // so returning early before writing would lose the exact args/envs we need for fat-link replay.
    write_rustc_args(&args_dir, &rustc_args);

    // If we are being asked to link, delegate to the linker action after capturing.
    if has_linking_args() {
        return crate::link::LinkAction::from_env()
            .expect("Linker action not found")
            .run_link();
    }

    // Run the actual rustc command.
    // We want all stdout/stderr to be inherited, so the user sees the compiler output.
    let mut cmd = rustc_args.replay();
    cmd.stdout(std::process::Stdio::inherit());
    cmd.stderr(std::process::Stdio::inherit());

    let status = cmd.status().expect("Failed to execute rustc command");
    std::process::exit(status.code().unwrap_or(1));
}

fn write_rustc_args(args_dir: &PathBuf, rustc_args: &RustcArgs) {
    let crate_name = rustc_args
        .args
        .iter()
        .skip_while(|arg| *arg != "--crate-name")
        .nth(1);

    if let Some(crate_name) = crate_name {
        if crate_name != "___" {
            std::fs::create_dir_all(args_dir)
                .expect("Failed to create args directory for rustc wrapper");

            let crate_type = rustc_args
                .args
                .iter()
                .skip_while(|arg| *arg != "--crate-type")
                .nth(1)
                .map(|s| s.as_str());

            let serialized_args =
                serde_json::to_string(rustc_args).expect("Failed to serialize rustc args");

            let suffix = match crate_type {
                Some("lib" | "rlib") => "lib",
                Some("bin") => "bin",
                _ => "bin",
            };

            std::fs::write(
                args_dir.join(format!("{crate_name}.{suffix}.json")),
                &serialized_args,
            )
            .expect("Failed to write rustc args to file");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RustcArgs;
    use std::path::PathBuf;

    #[test]
    fn replay_restores_program_args_env_and_cwd() {
        let rustc_args = RustcArgs {
            args: vec![
                "/toolchain/bin/rustc".into(),
                "--crate-name".into(),
                "example".into(),
            ],
            envs: vec![("FOO".into(), "BAR".into())],
            cwd: PathBuf::from("/tmp/example"),
            link_args: vec![],
        };

        let cmd = rustc_args.replay();

        assert_eq!(cmd.get_program(), "/toolchain/bin/rustc");
        assert_eq!(
            cmd.get_args()
                .map(|arg| arg.to_string_lossy())
                .collect::<Vec<_>>(),
            vec!["--crate-name", "example"]
        );
        assert_eq!(
            cmd.get_envs()
                .map(|(key, val)| (
                    key.to_string_lossy().into_owned(),
                    val.map(|val| val.to_string_lossy().into_owned())
                ))
                .collect::<Vec<_>>(),
            vec![("FOO".to_string(), Some("BAR".to_string()))]
        );
        assert_eq!(
            cmd.get_current_dir(),
            Some(PathBuf::from("/tmp/example").as_path())
        );
    }
}
