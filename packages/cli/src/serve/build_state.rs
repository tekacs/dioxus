use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    fs::File,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const DIRECTORY_ENV: &str = "DX_DEVLOOP_DIR";
const REQUEST_FILE: &str = "request.json";
const STATE_FILE: &str = "state.json";
const RECEIPTS: &str = "receipts";
const VERSION: u8 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Request {
    version: u8,
    request: String,
    created_at_ms: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Status {
    Starting,
    Building,
    Served,
    Failed,
    Superseded,
    Exited,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct State {
    version: u8,
    instance: String,
    pid: u32,
    generation: u64,
    request: Option<String>,
    status: Status,
    started_at_ms: u64,
    settled_at_ms: Option<u64>,
    detail: Option<String>,
}

/// Atomic, process-addressed renderer build state for an external development loop.
///
/// The ordinary dx TUI remains diagnostic output. When `DX_DEVLOOP_DIR` is set,
/// a controller writes one request to `request.json`, sends the ordinary manual
/// rebuild input, and waits for `state.json` to settle that exact request.
pub(crate) struct Journal {
    directory: Option<PathBuf>,
    state: State,
    active: bool,
}

impl Journal {
    pub(crate) fn from_env() -> Result<Self> {
        Self::at(env::var_os(DIRECTORY_ENV).map(PathBuf::from))
    }

    fn at(directory: Option<PathBuf>) -> Result<Self> {
        let started_at_ms = now_ms()?;
        let pid = std::process::id();
        let mut journal = Self {
            directory,
            state: State {
                version: VERSION,
                instance: format!("{started_at_ms}-{pid}"),
                pid,
                generation: 0,
                request: None,
                status: Status::Starting,
                started_at_ms,
                settled_at_ms: None,
                detail: None,
            },
            active: false,
        };
        if let Some(directory) = &journal.directory {
            fs::create_dir_all(directory)
                .with_context(|| format!("create dx dev-loop directory {}", directory.display()))?;
            journal.clear_receipts()?;
            remove_if_present(&directory.join(REQUEST_FILE))?;
            journal.publish()?;
        }
        Ok(journal)
    }

    pub(crate) fn begin_manual(&mut self) -> Result<()> {
        let request = self.claim_request()?;
        if self.active {
            self.settle(
                Status::Superseded,
                Some("a newer manual rebuild replaced this generation".into()),
            )?;
        }
        if request.is_some() {
            // The DevOps request lock makes a newer identified request proof
            // that no earlier controller remains entitled to its receipt.
            self.clear_receipts()?;
        }
        let started_at_ms = now_ms()?;
        self.state.generation = self.state.generation.saturating_add(1);
        self.state.request = request.map(|request| request.request);
        self.state.status = Status::Building;
        self.state.started_at_ms = started_at_ms;
        self.state.settled_at_ms = None;
        self.state.detail = None;
        self.active = true;
        self.publish()
    }

    pub(crate) fn served(&mut self) -> Result<()> {
        self.settle(Status::Served, None)
    }

    pub(crate) fn failed(&mut self, detail: impl Into<String>) -> Result<()> {
        self.settle(Status::Failed, Some(detail.into()))
    }

    pub(crate) fn exited(&mut self) -> Result<()> {
        self.settle(Status::Exited, None)
    }

    fn settle(&mut self, status: Status, detail: Option<String>) -> Result<()> {
        let settled_at_ms = now_ms()?;
        if !self.active {
            self.state.generation = self.state.generation.saturating_add(1);
            self.state.request = None;
            self.state.started_at_ms = settled_at_ms;
        }
        self.state.status = status;
        self.state.settled_at_ms = Some(settled_at_ms);
        self.state.detail = detail;
        if let (Some(directory), Some(request)) = (&self.directory, &self.state.request) {
            let receipts = directory.join(RECEIPTS);
            fs::create_dir_all(&receipts).with_context(|| {
                format!("create dx build receipt directory {}", receipts.display())
            })?;
            write_json_atomic(&receipts.join(format!("{request}.json")), &self.state)?;
        }
        self.active = false;
        self.publish()
    }

    fn claim_request(&self) -> Result<Option<Request>> {
        let Some(directory) = &self.directory else {
            return Ok(None);
        };
        let path = directory.join(REQUEST_FILE);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read dx rebuild request {}", path.display()));
            }
        };
        let request: Request = serde_json::from_slice(&bytes)
            .with_context(|| format!("decode dx rebuild request {}", path.display()))?;
        if request.version != VERSION {
            bail!(
                "unsupported dx rebuild request version {} in {}",
                request.version,
                path.display()
            );
        }
        if request.request.is_empty()
            || !request
                .request
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            bail!(
                "dx rebuild request in {} has invalid identity",
                path.display()
            );
        }
        remove_if_present(&path)?;
        Ok(Some(request))
    }

    fn publish(&mut self) -> Result<()> {
        let Some(directory) = &self.directory else {
            return Ok(());
        };
        write_json_atomic(&directory.join(STATE_FILE), &self.state)
    }

    fn clear_receipts(&self) -> Result<()> {
        let Some(directory) = &self.directory else {
            return Ok(());
        };
        let receipts = directory.join(RECEIPTS);
        match fs::remove_dir_all(&receipts) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error)
                .with_context(|| format!("remove dx build receipts {}", receipts.display())),
        }
    }
}

fn now_ms() -> Result<u64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_millis();
    u64::try_from(millis).context("system clock does not fit renderer state")
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("renderer state path {} has no parent", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create renderer state directory {}", parent.display()))?;
    let bytes = serde_json::to_vec(value).context("encode renderer build state")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create renderer state in {}", parent.display()))?;
    temporary
        .write_all(&bytes)
        .and_then(|()| temporary.write_all(b"\n"))
        .and_then(|()| temporary.as_file().sync_all())
        .with_context(|| format!("write renderer state {}", path.display()))?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("publish renderer state {}", path.display()))?;
    #[cfg(unix)]
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("sync renderer state directory {}", parent.display()))?;
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_request_is_claimed_and_settled_atomically() -> Result<()> {
        let directory = tempfile::tempdir().context("create fixture")?;
        let mut journal = Journal::at(Some(directory.path().to_owned()))?;
        let request = Request {
            version: VERSION,
            request: "request-1".into(),
            created_at_ms: now_ms()?,
        };
        write_json_atomic(&directory.path().join(REQUEST_FILE), &request)?;

        journal.begin_manual()?;
        let building: State =
            serde_json::from_slice(&fs::read(directory.path().join(STATE_FILE))?)?;
        assert_eq!(building.request.as_deref(), Some("request-1"));
        assert!(matches!(building.status, Status::Building));
        assert!(!directory.path().join(REQUEST_FILE).exists());

        journal.served()?;
        let served: State = serde_json::from_slice(&fs::read(directory.path().join(STATE_FILE))?)?;
        assert_eq!(served.generation, building.generation);
        assert_eq!(served.request, building.request);
        assert!(matches!(served.status, Status::Served));
        assert!(served.settled_at_ms.is_some());
        let receipt: State = serde_json::from_slice(&fs::read(
            directory.path().join(RECEIPTS).join("request-1.json"),
        )?)?;
        assert!(matches!(receipt.status, Status::Served));
        Ok(())
    }

    #[test]
    fn human_supersession_preserves_the_identified_waiters_receipt() -> Result<()> {
        let directory = tempfile::tempdir().context("create fixture")?;
        let mut journal = Journal::at(Some(directory.path().to_owned()))?;
        write_json_atomic(
            &directory.path().join(REQUEST_FILE),
            &Request {
                version: VERSION,
                request: "first".into(),
                created_at_ms: now_ms()?,
            },
        )?;
        journal.begin_manual()?;
        // No request file: this is a human pressing r while the identified
        // controller still owns the request lock.
        journal.begin_manual()?;

        let state: State = serde_json::from_slice(&fs::read(directory.path().join(STATE_FILE))?)?;
        assert_eq!(state.generation, 2);
        assert!(state.request.is_none());
        let first: State = serde_json::from_slice(&fs::read(
            directory.path().join(RECEIPTS).join("first.json"),
        )?)?;
        assert!(matches!(first.status, Status::Superseded));
        Ok(())
    }

    #[test]
    fn a_new_identified_request_reaps_an_orphaned_receipt() -> Result<()> {
        let directory = tempfile::tempdir().context("create fixture")?;
        let mut journal = Journal::at(Some(directory.path().to_owned()))?;
        for request in ["first", "second"] {
            write_json_atomic(
                &directory.path().join(REQUEST_FILE),
                &Request {
                    version: VERSION,
                    request: request.into(),
                    created_at_ms: now_ms()?,
                },
            )?;
            journal.begin_manual()?;
            journal.served()?;
        }
        assert!(!directory.path().join(RECEIPTS).join("first.json").exists());
        assert!(directory.path().join(RECEIPTS).join("second.json").exists());
        Ok(())
    }

    #[test]
    fn a_new_dx_instance_reaps_prior_instance_receipts() -> Result<()> {
        let directory = tempfile::tempdir().context("create fixture")?;
        let receipts = directory.path().join(RECEIPTS);
        fs::create_dir_all(&receipts)?;
        fs::write(receipts.join("orphan.json"), b"orphan")?;
        Journal::at(Some(directory.path().to_owned()))?;
        assert!(!receipts.exists());
        Ok(())
    }
}
