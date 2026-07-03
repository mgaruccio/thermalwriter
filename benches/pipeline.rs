//! Micro-benches for the tick-loop hot path: pixel rotation and JPEG encoding.
//!
//! `rotate_pixels`/`encode_jpeg` live in `service::tick`, which is gated by the
//! `daemon` feature (this bench target is too — see `required-features` in
//! Cargo.toml).
//!
//! Compare workflow: `cargo bench -- --save-baseline before`, make a change,
//! then `cargo bench -- --baseline before` to diff against the saved run.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use thermalwriter::render::RawFrame;
use thermalwriter::service::tick::{encode_jpeg, rotate_pixels};

const WIDTH: u32 = 480;
const HEIGHT: u32 = 480;

/// A deterministic gradient frame — real image content (not all-zero) so JPEG
/// encoding does representative entropy-coding work.
fn sample_frame() -> RawFrame {
    let mut data = Vec::with_capacity((WIDTH * HEIGHT * 3) as usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            data.push((x * 255 / WIDTH) as u8);
            data.push((y * 255 / HEIGHT) as u8);
            data.push(((x + y) * 255 / (WIDTH + HEIGHT)) as u8);
        }
    }
    RawFrame {
        data,
        width: WIDTH,
        height: HEIGHT,
    }
}

fn bench_rotate_pixels(c: &mut Criterion) {
    let frame = sample_frame();
    let mut group = c.benchmark_group("rotate_pixels");
    for degrees in [0u16, 90, 180, 270] {
        group.bench_with_input(
            BenchmarkId::from_parameter(degrees),
            &degrees,
            |b, &degrees| {
                b.iter(|| {
                    rotate_pixels(
                        black_box(&frame.data),
                        black_box(frame.width),
                        black_box(frame.height),
                        black_box(degrees),
                    )
                });
            },
        );
    }
    group.finish();
}

fn bench_encode_jpeg(c: &mut Criterion) {
    let frame = sample_frame();
    let mut group = c.benchmark_group("encode_jpeg");

    // The daemon's default quality, at its default 180° rotation.
    group.bench_function("quality_85_rotation_180", |b| {
        b.iter(|| encode_jpeg(black_box(&frame), black_box(85), black_box(180)).unwrap());
    });

    // Quality sweep (rotation fixed at 180° — matches the shipped default).
    for quality in [10u8, 50, 85, 100] {
        group.bench_with_input(
            BenchmarkId::new("quality_sweep", quality),
            &quality,
            |b, &quality| {
                b.iter(|| {
                    encode_jpeg(black_box(&frame), black_box(quality), black_box(180)).unwrap()
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_rotate_pixels, bench_encode_jpeg);
criterion_main!(benches);
