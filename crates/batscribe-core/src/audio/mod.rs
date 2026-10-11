//! Audio capture and conversion.
//!
//! The real-time callback only copies samples into a lock-free ring. A capture thread drains it,
//! downmixes and resamples to the transport rate (16 kHz mono PCM16 by default), measures level
//! and sends fixed-size batches with monotonic sample positions. If the ring overflows, the
//! lost frames are replaced with silence (keeping the timeline intact) and reported.

pub mod resample;
pub mod wav;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc;

use resample::Resampler;

#[derive(Debug, Clone)]
pub struct AudioChunk {
    /// Position of the first sample on the recording's transport-rate timeline.
    pub first_sample: u64,
    pub samples: Vec<i16>,
}

impl AudioChunk {
    pub fn end_sample(&self) -> u64 {
        self.first_sample + self.samples.len() as u64
    }
}

#[derive(Debug)]
pub enum AudioEvent {
    Chunk(AudioChunk),
    /// 0..1, roughly -60..0 dBFS.
    Level(f32),
    /// Frames lost before conversion (replaced with silence at `at_sample`).
    Overflow {
        at_sample: u64,
        frames: u64,
    },
    Error(String),
    /// Capture stopped and everything captured was sent.
    Ended,
}

#[derive(Debug, Clone)]
pub struct CaptureInfo {
    pub device: String,
    pub device_rate: u32,
    pub device_channels: u16,
    pub sample_format: String,
}

pub struct Capture {
    stop: Arc<AtomicBool>,
}

impl Capture {
    /// Stops capture; the remaining audio is sent, followed by `Ended`.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Input devices as (stable id, label). Output monitors are listed after real inputs.
pub fn input_devices() -> Vec<(String, String)> {
    let host = cpal::default_host();
    let Ok(devices) = host.input_devices() else {
        return Vec::new();
    };
    let mut list: Vec<(bool, String, String)> = devices
        .filter_map(|d| {
            let id = d.id().ok()?.to_string();
            let name = d
                .description()
                .map(|desc| desc.name().to_owned())
                .unwrap_or_else(|_| id.clone());
            let monitor = id.contains("sink") || id.contains("output");
            let label = match name.as_str() {
                "default_input" => "Default input".to_owned(),
                "default_sink" => "Monitor of the default output".to_owned(),
                _ if monitor => format!("Monitor of {name}"),
                _ => name,
            };
            Some((monitor, id, label))
        })
        .collect();
    list.sort_by_key(|(monitor, _, _)| *monitor);
    list.into_iter().map(|(_, id, label)| (id, label)).collect()
}

pub fn level(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples
        .iter()
        .map(|&s| (f64::from(s) / 32768.0).powi(2))
        .sum();
    let rms = (sum / samples.len() as f64).sqrt();
    let db = 20.0 * rms.max(1e-9).log10();
    ((db + 60.0) / 60.0).clamp(0.0, 1.0) as f32
}

/// Starts capturing from `device_id` ("" for the default device, "synthetic" for generated
/// silence). Batches of `batch_ms` at `rate` Hz go to `tx`.
pub fn start(
    device_id: &str,
    rate: u32,
    batch_ms: u32,
    tx: mpsc::Sender<AudioEvent>,
) -> Result<(Capture, CaptureInfo), String> {
    let batch = (rate as u64 * batch_ms.max(10) as u64 / 1000).max(1) as usize;
    if device_id == "synthetic" {
        return Ok(synthetic(rate, batch, tx));
    }
    let host = cpal::default_host();
    let device = if device_id.is_empty() {
        host.default_input_device()
            .ok_or("No microphone is available.")?
    } else {
        let id = device_id
            .parse()
            .map_err(|_| format!("Unknown audio device {device_id:?}"))?;
        host.device_by_id(&id)
            .ok_or("The selected microphone isn't available. Choose another in Preferences.")?
    };
    let name = device
        .description()
        .map(|d| d.name().to_owned())
        .unwrap_or_else(|_| device_id.to_owned());
    let config = device
        .default_input_config()
        .map_err(|e| format!("The microphone has no usable input configuration: {e}"))?;
    let info = CaptureInfo {
        device: name,
        device_rate: config.sample_rate() as u32,
        device_channels: config.channels(),
        sample_format: format!("{:?}", config.sample_format()),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let thread_stop = stop.clone();
    let thread_info = info.clone();
    std::thread::Builder::new()
        .name("batscribe-audio".into())
        .spawn(move || {
            // The stream must live on this thread; it is dropped when capture ends.
            let channels = thread_info.device_channels.max(1) as usize;
            let capacity = thread_info.device_rate as usize * channels * 2; // ~2 s
            let (producer, consumer) = rtrb::RingBuffer::<f32>::new(capacity.max(4096));
            let overflow = Arc::new(AtomicU64::new(0));
            let stream = build_stream(
                &device,
                &config,
                producer,
                overflow.clone(),
                tx.clone(),
                thread_stop.clone(),
            );
            let stream = match stream {
                Ok(s) => s,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            if let Err(e) = stream.play() {
                let _ = ready_tx.send(Err(format!("Couldn't start the microphone: {e}")));
                return;
            }
            let _ = ready_tx.send(Ok(()));
            pump(
                consumer,
                overflow,
                channels,
                thread_info.device_rate,
                rate,
                batch,
                &thread_stop,
                &tx,
            );
            drop(stream);
            let _ = tx.blocking_send(AudioEvent::Ended);
        })
        .map_err(|e| e.to_string())?;
    match ready_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => Ok((Capture { stop }, info)),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("The microphone didn't start in time.".into()),
    }
}

fn build_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    mut producer: rtrb::Producer<f32>,
    overflow: Arc<AtomicU64>,
    tx: mpsc::Sender<AudioEvent>,
    stop: Arc<AtomicBool>,
) -> Result<cpal::Stream, String> {
    let stream_config: cpal::StreamConfig = (*config).into();
    let err_tx = tx.clone();
    let on_error = move |e: cpal::Error| {
        // Tearing the stream down can report a disconnect; that's not an error.
        if !stop.load(Ordering::SeqCst) {
            let _ = err_tx.try_send(AudioEvent::Error(format!("Microphone error: {e}")));
        }
    };
    macro_rules! stream {
        ($t:ty, $conv:expr) => {
            device.build_input_stream(
                stream_config,
                move |data: &[$t], _: &cpal::InputCallbackInfo| {
                    // Real-time: copy only. Lost samples are counted, never waited for.
                    let mut lost = 0u64;
                    for &s in data {
                        if producer.push($conv(s)).is_err() {
                            lost += 1;
                        }
                    }
                    if lost > 0 {
                        overflow.fetch_add(lost, Ordering::Relaxed);
                    }
                },
                on_error,
                None,
            )
        };
    }
    let result = match config.sample_format() {
        cpal::SampleFormat::F32 => stream!(f32, |s: f32| s),
        cpal::SampleFormat::I16 => stream!(i16, |s: i16| f32::from(s) / 32768.0),
        cpal::SampleFormat::I32 => stream!(i32, |s: i32| s as f32 / 2_147_483_648.0),
        cpal::SampleFormat::U16 => stream!(u16, |s: u16| (f32::from(s) - 32768.0) / 32768.0),
        other => return Err(format!("Unsupported microphone sample format {other:?}")),
    };
    result.map_err(|e| format!("Couldn't open the microphone: {e}"))
}

#[allow(clippy::too_many_arguments)]
fn pump(
    mut consumer: rtrb::Consumer<f32>,
    overflow: Arc<AtomicU64>,
    channels: usize,
    device_rate: u32,
    rate: u32,
    batch: usize,
    stop: &AtomicBool,
    tx: &mpsc::Sender<AudioEvent>,
) {
    let mut resampler = Resampler::new(device_rate, rate);
    let mut pending: Vec<i16> = Vec::with_capacity(batch * 2);
    let mut position = 0u64;
    let mut frame = Vec::with_capacity(channels);
    let mut mono: Vec<f32> = Vec::new();
    let send_batches = |pending: &mut Vec<i16>, position: &mut u64, all: bool| -> bool {
        while pending.len() >= batch || (all && !pending.is_empty()) {
            let n = batch.min(pending.len());
            let samples: Vec<i16> = pending.drain(..n).collect();
            let _ = tx.try_send(AudioEvent::Level(level(&samples)));
            let chunk = AudioChunk {
                first_sample: *position,
                samples,
            };
            *position = chunk.end_sample();
            if tx.blocking_send(AudioEvent::Chunk(chunk)).is_err() {
                return false;
            }
        }
        true
    };
    loop {
        let stopping = stop.load(Ordering::SeqCst);
        mono.clear();
        while let Ok(s) = consumer.pop() {
            frame.push(s);
            if frame.len() == channels {
                mono.push(frame.iter().sum::<f32>() / channels as f32);
                frame.clear();
            }
        }
        let lost = overflow.swap(0, Ordering::Relaxed) / channels as u64;
        if lost > 0 {
            let silence = (lost * rate as u64 / device_rate.max(1) as u64) as usize;
            let at = position + pending.len() as u64;
            let _ = tx.try_send(AudioEvent::Overflow {
                at_sample: at,
                frames: lost,
            });
            pending.extend(std::iter::repeat_n(0i16, silence));
        }
        for s in resampler.process(&mono) {
            pending.push((s.clamp(-1.0, 1.0) * 32767.0) as i16);
        }
        if !send_batches(&mut pending, &mut position, stopping) {
            return;
        }
        if stopping {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn synthetic(rate: u32, batch: usize, tx: mpsc::Sender<AudioEvent>) -> (Capture, CaptureInfo) {
    let stop = Arc::new(AtomicBool::new(false));
    let task_stop = stop.clone();
    let period = Duration::from_micros(batch as u64 * 1_000_000 / rate.max(1) as u64);
    tokio::spawn(async move {
        let mut position = 0u64;
        let mut ticker = tokio::time::interval(period);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
        loop {
            ticker.tick().await;
            if task_stop.load(Ordering::SeqCst) {
                break;
            }
            let _ = tx.try_send(AudioEvent::Level(0.05));
            let chunk = AudioChunk {
                first_sample: position,
                samples: vec![0; batch],
            };
            position = chunk.end_sample();
            if tx.send(AudioEvent::Chunk(chunk)).await.is_err() {
                return;
            }
        }
        let _ = tx.send(AudioEvent::Ended).await;
    });
    (
        Capture { stop },
        CaptureInfo {
            device: "Synthetic silence".into(),
            device_rate: rate,
            device_channels: 1,
            sample_format: "I16".into(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_scale() {
        assert_eq!(level(&[]), 0.0);
        assert_eq!(level(&[0; 100]), 0.0);
        assert!(level(&[i16::MAX; 100]) > 0.99);
        let quiet = level(&[300; 100]);
        assert!(quiet > 0.0 && quiet < 0.5);
    }

    #[tokio::test(start_paused = true)]
    async fn synthetic_source_is_contiguous() {
        let (tx, mut rx) = mpsc::channel(64);
        let (capture, _) = start("synthetic", 16_000, 80, tx).unwrap();
        let mut next = 0;
        for _ in 0..5 {
            loop {
                match rx.recv().await.unwrap() {
                    AudioEvent::Chunk(c) => {
                        assert_eq!(c.first_sample, next);
                        assert_eq!(c.samples.len(), 1280);
                        next = c.end_sample();
                        break;
                    }
                    _ => continue,
                }
            }
        }
        capture.stop();
        loop {
            if let AudioEvent::Ended = rx.recv().await.unwrap() {
                break;
            }
        }
    }
}
