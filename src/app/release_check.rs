//! One-shot check for a newer published Incline release.
//!
//! The release service publishes the latest version separately. This check is
//! deliberately advisory: network and parsing failures
//! are logged, but never interrupt startup or produce an error dialog.

use std::{sync::mpsc, time::Duration};

use anyhow::{Context, Result};
use semver::Version;

use super::App;

const LATEST_RELEASE_URL: &str = "https://inclinedesign.net/releases/latest.txt";

fn fetch_newer_release(current: &str) -> Result<Option<String>> {
    let config = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(5))).build();
    let agent: ureq::Agent = config.into();
    let mut response = agent
        .get(LATEST_RELEASE_URL)
        .header("User-Agent", concat!("Incline/", env!("CARGO_PKG_VERSION")))
        .call()
        .context("requesting the latest release")?;
    let body = response
        .body_mut()
        .with_config()
        .limit(128)
        .read_to_string()
        .context("reading the latest release response")?;

    newer_release(current, &body)
}

fn newer_release(current: &str, published: &str) -> Result<Option<String>> {
    let current = Version::parse(current).context("the current Incline version is not valid semantic versioning")?;
    let published = Version::parse(published.trim()).context("the website returned an invalid release version")?;
    Ok((published > current).then(|| published.to_string()))
}

impl<'a> App<'a> {
    pub(super) fn start_release_check(&mut self) {
        if self.pending_release_check.is_some() {
            return;
        }

        let (tx, rx) = mpsc::channel();
        let window = self.window.clone();
        crate::app::jobs::spawn_io_task(move || {
            let _ = tx.send(fetch_newer_release(crate::APP_RELEASE));
            if let Some(window) = window {
                window.request_redraw();
            }
        });
        self.pending_release_check = Some(rx);
    }

    pub(super) fn poll_release_check(&mut self) {
        let Some(receiver) = self.pending_release_check.as_ref() else {
            return;
        };

        match receiver.try_recv() {
            Ok(Ok(newer_release)) => {
                self.editor.newer_release = newer_release;
                self.pending_release_check = None;
            }
            Ok(Err(error)) => {
                log::warn!("Could not check for a newer Incline release: {error:#}");
                self.pending_release_check = None;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                log::warn!("Could not check for a newer Incline release: the background task ended unexpectedly");
                self.pending_release_check = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
}
