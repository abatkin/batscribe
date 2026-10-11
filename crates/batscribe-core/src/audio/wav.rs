//! Retained audio: one 16-bit mono WAV per recording, written on its own thread so file I/O
//! never delays recognition. The header is rewritten periodically; after a crash,
//! [`repair`] fixes it from the file's actual length.

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub enum WavMsg {
    Samples(Vec<i16>),
    Finish,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WavStatus {
    /// Samples written and flushed so far.
    Progress(u64),
    Finished(u64),
    Failed {
        samples: u64,
        message: String,
    },
}

/// Starts a writer for `path`. Status updates go to `status`.
pub fn start(
    path: &Path,
    rate: u32,
    status: impl Fn(WavStatus) + Send + 'static,
) -> Result<mpsc::Sender<WavMsg>, String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("Couldn't create {}: {e}", dir.display()))?;
    }
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .map_err(|e| format!("Couldn't create {}: {e}", path.display()))?;
    let (tx, rx) = mpsc::channel::<WavMsg>();
    std::thread::Builder::new()
        .name("batscribe-wav".into())
        .spawn(move || {
            let mut written = 0u64;
            let mut last_flush = Instant::now();
            let fail = |written, e: hound::Error| WavStatus::Failed {
                samples: written,
                message: e.to_string(),
            };
            while let Ok(msg) = rx.recv() {
                match msg {
                    WavMsg::Samples(samples) => {
                        for s in &samples {
                            if let Err(e) = writer.write_sample(*s) {
                                status(fail(written, e));
                                return;
                            }
                        }
                        written += samples.len() as u64;
                        if last_flush.elapsed() >= Duration::from_secs(2) {
                            if let Err(e) = writer.flush() {
                                status(fail(written, e));
                                return;
                            }
                            last_flush = Instant::now();
                            status(WavStatus::Progress(written));
                        }
                    }
                    WavMsg::Finish => break,
                }
            }
            match writer.finalize() {
                Ok(()) => status(WavStatus::Finished(written)),
                Err(e) => status(fail(written, e)),
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(tx)
}

pub fn path_for(dir: &Path, recording_id: &str) -> PathBuf {
    dir.join(format!("{recording_id}.wav"))
}

/// Fixes the RIFF and data sizes of a 16-bit mono WAV written by [`start`] from the file's
/// length. Returns the number of whole samples in the file.
pub fn repair(path: &Path) -> Result<u64, String> {
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    let mut header = [0u8; 44];
    f.read_exact(&mut header)
        .map_err(|_| "file is too short".to_owned())?;
    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" || &header[36..40] != b"data" {
        return Err("not a WAV file written by Batscribe".into());
    }
    let data = (len - 44) & !1;
    f.seek(SeekFrom::Start(4)).map_err(|e| e.to_string())?;
    f.write_all(&((36 + data) as u32).to_le_bytes())
        .map_err(|e| e.to_string())?;
    f.seek(SeekFrom::Start(40)).map_err(|e| e.to_string())?;
    f.write_all(&(data as u32).to_le_bytes())
        .map_err(|e| e.to_string())?;
    Ok(data / 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_repairs() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_for(dir.path(), "rec");
        let (stx, srx) = std::sync::mpsc::channel();
        let tx = start(&path, 16_000, move |s| {
            let _ = stx.send(s);
        })
        .unwrap();
        tx.send(WavMsg::Samples(vec![1, 2, 3, 4])).unwrap();
        tx.send(WavMsg::Finish).unwrap();
        assert_eq!(srx.recv().unwrap(), WavStatus::Finished(4));
        let r = hound::WavReader::open(&path).unwrap();
        assert_eq!(r.len(), 4);

        // Simulate a crash: append samples without updating the header.
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&[5, 0, 6, 0, 7]).unwrap();
        drop(f);
        assert_eq!(repair(&path).unwrap(), 6);
        let r = hound::WavReader::open(&path).unwrap();
        assert_eq!(r.len(), 6);
    }
}
