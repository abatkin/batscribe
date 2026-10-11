//! Lists input devices and captures a couple of seconds from one, printing levels.
//! No network, no files.
//!
//!   cargo run -p batscribe-core --example mic_check [device-id] [seconds]

use batscribe_core::audio::{self, AudioEvent};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    println!("Input devices:");
    for (id, label) in audio::input_devices() {
        println!("  {id}  ({label})");
    }
    let device = args.get(1).cloned().unwrap_or_default();
    let seconds: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(2.0);
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let (capture, info) = match audio::start(&device, 16_000, 80, tx) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("capture failed: {e}");
            std::process::exit(1);
        }
    };
    println!("Capturing from {info:?}");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs_f64(seconds);
    let (mut samples, mut peak, mut next) = (0u64, 0f32, 0u64);
    let mut stopping = false;
    loop {
        let ev = tokio::select! {
            ev = rx.recv() => ev,
            _ = tokio::time::sleep_until(deadline), if !stopping => {
                stopping = true;
                capture.stop();
                continue;
            }
        };
        match ev {
            Some(AudioEvent::Chunk(c)) => {
                assert_eq!(c.first_sample, next, "contiguous sample positions");
                next = c.end_sample();
                samples += c.samples.len() as u64;
            }
            Some(AudioEvent::Level(l)) => peak = peak.max(l),
            Some(AudioEvent::Overflow { frames, .. }) => println!("overflow: {frames} frames"),
            Some(AudioEvent::Error(e)) => println!("error: {e}"),
            Some(AudioEvent::Ended) | None => break,
        }
    }
    println!(
        "{samples} samples at 16 kHz ({:.2} s), peak level {peak:.2}",
        samples as f64 / 16_000.0
    );
}
