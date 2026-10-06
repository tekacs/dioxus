use futures_channel::mpsc::UnboundedReceiver;
use futures_util::StreamExt;
use notify::Event;
use std::{path::PathBuf, time::Duration};

/// A cancellable wait must not yield after removing notifications from the channel.
pub(super) async fn next(events: &mut UnboundedReceiver<Event>) -> Vec<PathBuf> {
    let first = events.next().await;
    let mut paths: Vec<_> = first.into_iter().flat_map(|event| event.paths).collect();
    while let Ok(event) = events.try_recv() {
        paths.extend(event.paths);
    }
    paths
}

/// Called by the batch's handler, outside the serve loop's cancellable selection.
pub(super) async fn settle(paths: &[PathBuf]) {
    let transient = paths.iter().any(|path| match std::fs::metadata(path) {
        Ok(metadata) => metadata.is_file() && metadata.len() == 0,
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    });
    if transient {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_channel::mpsc::unbounded;
    use futures_util::{FutureExt, poll};
    use notify::EventKind;

    #[tokio::test]
    async fn transient_batch_wins_before_cancellation() {
        let directory = tempfile::tempdir().unwrap();
        let empty = directory.path().join("empty.rs");
        let missing = directory.path().join(".renamed-temp");
        let target = directory.path().join("target.rs");
        std::fs::write(&empty, "").unwrap();
        std::fs::write(&target, "fn updated() {}").unwrap();
        let (sender, mut events) = unbounded();
        sender
            .unbounded_send(Event::new(EventKind::Any).add_path(empty.clone()))
            .unwrap();
        sender
            .unbounded_send(
                Event::new(EventKind::Any)
                    .add_path(missing.clone())
                    .add_path(target.clone()),
            )
            .unwrap();

        // Models a competing log/socket event in serve_all. Sleeping after receipt
        // lets that branch win and destroys the already-dequeued file batch.
        let batch = tokio::select! {
            biased;
            batch = next(&mut events) => Some(batch),
            () = async {} => None,
        };
        assert_eq!(batch, Some(vec![empty, missing, target]));
    }

    #[tokio::test]
    async fn cancelled_idle_wait_leaves_channel_usable() {
        let (sender, mut events) = unbounded();
        let mut waiting = Box::pin(next(&mut events));
        assert!(poll!(waiting.as_mut()).is_pending());
        drop(waiting);

        let path = PathBuf::from("changed.rs");
        sender
            .unbounded_send(Event::new(EventKind::Any).add_path(path.clone()))
            .unwrap();
        assert_eq!(next(&mut events).now_or_never(), Some(vec![path]));
    }

    #[tokio::test]
    async fn settling_keeps_the_owned_batch() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("changing.rs");
        std::fs::write(&path, "").unwrap();
        let paths = vec![path.clone()];
        let mut settling = Box::pin(settle(&paths));
        assert!(poll!(settling.as_mut()).is_pending());
        std::fs::write(&path, "fn complete() {}").unwrap();
        settling.await;
        assert_eq!(paths, vec![path]);
        assert_eq!(settle(&paths).now_or_never(), Some(()));
    }
}
