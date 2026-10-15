//! Development helpers, platform-neutral: record every `Input` as JSON lines,
//! and play a recording back instead of connecting to any platform.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::model::Input;
use crate::pipeline::now_ms;
use crate::providers::Provider;
use crate::services::Services;

/// Longest pause between replayed inputs, whatever the recording says.
const MAX_GAP: Duration = Duration::from_secs(10);

pub struct Recorder {
    path: PathBuf,
    file: BufWriter<File>,
    failed: bool,
}

impl Recorder {
    pub fn create(path: &Path) -> anyhow::Result<Recorder> {
        let file = File::create(path)
            .map_err(|error| anyhow::anyhow!("couldn't create {}: {error}", path.display()))?;
        tracing::info!(path = %path.display(), "recording inputs");
        Ok(Recorder { path: path.to_path_buf(), file: BufWriter::new(file), failed: false })
    }

    pub fn record(&mut self, input: &Input) {
        let result = serde_json::to_writer(&mut self.file, input)
            .map_err(std::io::Error::other)
            .and_then(|()| self.file.write_all(b"\n"))
            .and_then(|()| self.file.flush());
        if let Err(error) = result
            && !self.failed
        {
            tracing::error!(path = %self.path.display(), %error, "couldn't write the recording");
            self.failed = true;
        }
    }
}

pub struct Replay {
    path: PathBuf,
    speed: f64,
}

impl Replay {
    pub fn new(path: PathBuf, speed: f64) -> Replay {
        Replay { path, speed: if speed > 0.0 { speed } else { 1.0 } }
    }
}

impl Provider for Replay {
    fn spawn(self: Box<Self>, inputs: mpsc::Sender<Input>, _services: Services) -> JoinHandle<()> {
        tokio::spawn(async move {
            let text = match tokio::fs::read_to_string(&self.path).await {
                Ok(text) => text,
                Err(error) => {
                    tracing::error!(path = %self.path.display(), %error, "couldn't read the recording");
                    return;
                }
            };
            tracing::info!(path = %self.path.display(), speed = self.speed, "replaying inputs");
            let mut previous_at: Option<u64> = None;
            for (number, line) in text.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                let mut input: Input = match serde_json::from_str(line) {
                    Ok(input) => input,
                    Err(error) => {
                        tracing::warn!(line = number + 1, %error, "skipping a line that isn't an Input");
                        continue;
                    }
                };
                if let Some(previous) = previous_at {
                    let gap = Duration::from_millis(input.at_ms.saturating_sub(previous)).div_f64(self.speed);
                    tokio::time::sleep(gap.min(MAX_GAP)).await;
                }
                previous_at = Some(input.at_ms);
                // Replayed inputs happen now, as far as the pipeline knows.
                input.at_ms = now_ms();
                if inputs.send(input).await.is_err() {
                    return;
                }
            }
            tracing::info!("replay finished");
        })
    }
}
