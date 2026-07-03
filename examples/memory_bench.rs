//! Memory-usage benchmark for the render pipeline.
//!
//! Replicates the daemon's steady-state render loop headlessly — SVG layout,
//! mock sensors, history, background compositing, JPEG encoding — and reports
//! RSS, peak RSS, in-use allocator bytes, and allocation churn so autoresearch
//! can optimise memory without regressing CPU.
//!
//! Usage:
//!   cargo run --release --example memory_bench [frames] [warmup]
//!
//! Defaults: 200 measurement frames, 50 warmup frames.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use thermalwriter::config::builtin_layouts;
use thermalwriter::render::background::decode_to_pixmap;
use thermalwriter::render::frontmatter::LayoutFrontmatter;
use thermalwriter::render::svg::SvgRenderer;
use thermalwriter::render::FrameSource;
use thermalwriter::sensor::history::SensorHistory;
use thermalwriter::sensor::mock::{fill_synthetic_history, mock_sensors, mock_sensors_varying};
use thermalwriter::service::tick::encode_jpeg;
use thermalwriter::theme::ThemePalette;

// ---------------------------------------------------------------------------
// Counting allocator — wraps System, tracks total allocated + deallocated bytes.
// This captures allocation CHURN (per-frame traffic) that mallinfo/RSS hide
// when the allocator reuses freed blocks.
// ---------------------------------------------------------------------------

struct CountingAllocator;

static TOTAL_ALLOCATED: AtomicU64 = AtomicU64::new(0);
static TOTAL_DEALLOCATED: AtomicU64 = AtomicU64::new(0);
static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
static DEALLOC_COUNT: AtomicU64 = AtomicU64::new(0);

// SAFETY: forwards to System which is Send+Sync. The atomics are independently safe.
unsafe impl Sync for CountingAllocator {}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarding to System with the same layout.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            TOTAL_ALLOCATED.fetch_add(layout.size() as u64, Ordering::Relaxed);
            ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarding to System with the same ptr+layout.
        unsafe { System.dealloc(ptr, layout) };
        TOTAL_DEALLOCATED.fetch_add(layout.size() as u64, Ordering::Relaxed);
        DEALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarding to System.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            TOTAL_ALLOCATED.fetch_add(layout.size() as u64, Ordering::Relaxed);
            ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn realloc(&self, old_ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarding to System. realloc is modeled as dealloc old + alloc new
        // for churn tracking (the old bytes are "freed" and new bytes "allocated").
        let old_size = layout.size();
        // SAFETY: forwarding to System.
        let ptr = unsafe { System.realloc(old_ptr, layout, new_size) };
        if !ptr.is_null() {
            TOTAL_DEALLOCATED.fetch_add(old_size as u64, Ordering::Relaxed);
            TOTAL_ALLOCATED.fetch_add(new_size as u64, Ordering::Relaxed);
            // realloc counts as one dealloc + one alloc.
            DEALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        ptr
    }
}

#[global_allocator]
static ALLOC: CountingAllocator = CountingAllocator;

fn reset_counters() {
    TOTAL_ALLOCATED.store(0, Ordering::Relaxed);
    TOTAL_DEALLOCATED.store(0, Ordering::Relaxed);
    ALLOC_COUNT.store(0, Ordering::Relaxed);
    DEALLOC_COUNT.store(0, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// /proc/self/status readers
// ---------------------------------------------------------------------------

fn vm_rss_kb() -> u64 {
    read_proc_status_field("VmRSS")
}

fn vm_hwm_kb() -> u64 {
    read_proc_status_field("VmHWM")
}

fn read_proc_status_field(field: &str) -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix(field) {
            if let Some(rest) = rest.strip_prefix(':') {
                return rest
                    .trim()
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
            }
        }
    }
    0
}

/// Live heap in-use from glibc mallinfo (uordblks). Independent cross-check
/// against the counting allocator's net (allocated - deallocated).
fn mallinfo_in_use_bytes() -> u64 {
    #[cfg(target_env = "gnu")]
    {
        let info = unsafe { libc::mallinfo() };
        info.uordblks as u64
    }
    #[cfg(not(target_env = "gnu"))]
    {
        0
    }
}

// ---------------------------------------------------------------------------
// Renderer setup — mirrors benches/render.rs build_renderer + background
// ---------------------------------------------------------------------------

fn build_renderer(
    template: &str,
    sensors: &thermalwriter::render::SensorData,
) -> SvgRenderer<'static> {
    let frontmatter = LayoutFrontmatter::parse(template);
    let mut renderer = SvgRenderer::new(template, 480, 480).expect("valid built-in layout");
    renderer.set_theme(ThemePalette::default());

    if !frontmatter.history_configs.is_empty() {
        let metrics: Vec<String> = frontmatter.history_configs.keys().cloned().collect();
        let mut history = SensorHistory::new();
        for (metric, cfg) in &frontmatter.history_configs {
            history.configure_metric(metric, cfg.duration);
        }
        fill_synthetic_history(&mut history, &metrics, sensors);
        renderer.set_history(Arc::new(Mutex::new(history)));
    }

    // Decode the built-in background pixmap (same one the daemon loads at startup).
    let bg = decode_to_pixmap(builtin_layouts::BG_DARK_GRADIENT)
        .expect("built-in background must decode");
    renderer.set_background(Some(bg));

    renderer
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let measure_frames: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(200);
    let warmup_frames: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(50);

    // Deterministic mock sensor data (same every run).
    let sensors = mock_sensors();

    // Build the renderer exactly as the daemon does for the default layout.
    let mut renderer = build_renderer(builtin_layouts::SVG_NEON_DASH_V2, &sensors);

    // ---- Warmup phase: let allocator settle, font cache warm, etc. ----
    for i in 0..warmup_frames {
        let s = mock_sensors_varying(i);
        let frame = renderer.render(&s).expect("render must succeed");
        let _jpeg = encode_jpeg(&frame, 85, 180).expect("jpeg encode must succeed");
    }

    // ---- Pre-measurement snapshot ----
    let rss_before = vm_rss_kb();
    let hwm_before = vm_hwm_kb();
    let mallinfo_before = mallinfo_in_use_bytes();
    reset_counters(); // zero the counting allocator for the measurement window

    // ---- Measurement loop ----
    let start = std::time::Instant::now();
    let mut jpeg_bytes_total: u64 = 0;

    for i in 0..measure_frames {
        let s = mock_sensors_varying(warmup_frames + i);
        let frame = renderer.render(&s).expect("render must succeed");
        let jpeg = encode_jpeg(&frame, 85, 180).expect("jpeg encode must succeed");
        jpeg_bytes_total += jpeg.len() as u64;
        drop(frame);
        drop(jpeg);
    }

    let elapsed = start.elapsed();

    // ---- Post-measurement snapshot ----
    let rss_after = vm_rss_kb();
    let hwm_after = vm_hwm_kb();
    let mallinfo_after = mallinfo_in_use_bytes();

    // Counting allocator totals for the measurement window.
    let total_allocated = TOTAL_ALLOCATED.load(Ordering::Relaxed);
    let total_deallocated = TOTAL_DEALLOCATED.load(Ordering::Relaxed);
    let alloc_count = ALLOC_COUNT.load(Ordering::Relaxed);
    let dealloc_count = DEALLOC_COUNT.load(Ordering::Relaxed);

    let steady_rss_kb = rss_after;
    let peak_rss_kb = hwm_after;
    // fps omitted: the counting allocator's atomics contaminate wall-clock timing.
    // CPU throughput is measured separately by cpu_bench (uninstrumented allocator).
    let churn_bytes_per_frame = total_allocated / measure_frames;
    let live_net_bytes = total_allocated.saturating_sub(total_deallocated);
    let avg_jpeg_bytes = jpeg_bytes_total / measure_frames;

    // ---- Emit METRIC lines ----
    // Primary: steady-state RSS (KB). Lower is better.
    println!("METRIC rss_kb={}", steady_rss_kb);
    // Secondary: peak RSS high-water mark (KB).
    println!("METRIC peak_rss_kb={}", peak_rss_kb);
    // Secondary: allocation churn per frame (bytes). Total bytes allocated / frames.
    println!("METRIC churn_bytes_per_frame={}", churn_bytes_per_frame);
    // Secondary: live net allocation growth during measurement (bytes). ~0 means no leak.
    println!("METRIC live_net_bytes={}", live_net_bytes);
    // Secondary: live heap from mallinfo (bytes). Independent cross-check.
    println!("METRIC mallinfo_in_use_bytes={}", mallinfo_after);
    // Secondary: average JPEG output size.
    println!("METRIC avg_jpeg_bytes={}", avg_jpeg_bytes);

    // Diagnostic output to stderr.
    eprintln!("--- memory_bench summary ---");
    eprintln!("warmup frames:    {}", warmup_frames);
    eprintln!("measure frames:   {}", measure_frames);
    eprintln!("elapsed:           {:.3}s", elapsed.as_secs_f64());
    eprintln!("rss before:        {} KB", rss_before);
    eprintln!("rss after:         {} KB", rss_after);
    eprintln!("peak rss (HWM):    {} KB (was {} KB before measure)", peak_rss_kb, hwm_before);
    eprintln!("mallinfo before:   {} bytes", mallinfo_before);
    eprintln!("mallinfo after:    {} bytes", mallinfo_after);
    eprintln!("total allocated:   {} bytes ({} allocs)", total_allocated, alloc_count);
    eprintln!("total deallocated: {} bytes ({} deallocs)", total_deallocated, dealloc_count);
    eprintln!("churn/frame:       {} bytes", churn_bytes_per_frame);
    eprintln!("live net growth:   {} bytes", live_net_bytes);
    eprintln!("avg jpeg size:     {} bytes", avg_jpeg_bytes);
}
