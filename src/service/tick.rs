// SPDX-License-Identifier: GPL-3.0-or-later
//
// Tick loop: polls sensors, renders a frame, encodes, sends via transport.
// Connection generations and source revisions commit only after matching
// SourceBuildResults land.

use anyhow::Result;
use log::{debug, info, warn};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use crate::config::MediaConfig;
use crate::render::background::BackgroundImage;
use crate::render::{FrameSource, RawFrame};
use crate::sensor::SensorHub;
use crate::sensor::history::SensorHistory;
use crate::sensor::mpris::{self, MediaSnapshot};
use crate::service::frame_dump;
use crate::service::mode_handler::RuntimeDisplayDimensions;
use crate::transport::discovery::TransportConnector;
use crate::transport::encode::encode_frame;
use crate::transport::{DeviceInfo, EncodedFrame, Transport};

pub use crate::transport::encode::rotate_pixels;

/// Request that the mode listener rebuild the frame source for a generation.
#[derive(Debug, Clone, Copy)]
pub struct SourceBuildRequest {
    pub generation: u64,
    pub width: u32,
    pub height: u32,
}

/// Result of a generation- and source-revision-tagged source rebuild (layout
/// swap or reconnect).
pub struct SourceBuildResult {
    pub generation: u64,
    pub source_revision: u64,
    pub source: Result<Box<dyn FrameSource>, String>,
    /// Optional acknowledgement for D-Bus mode changes. Sent only after this
    /// generation and source revision are accepted and the source is committed
    /// (or rejected).
    pub commit: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
}
/// Advance the source revision and invalidate queued builds before a mode
/// change is acknowledged.
pub struct SourceRevisionApply {
    pub revision: u64,
    pub reset_connection: bool,
    pub ack: tokio::sync::oneshot::Sender<Result<(), String>>,
}

/// End-to-end background apply request; acked only after set_background succeeds.
pub struct BackgroundApply {
    pub image: Option<Arc<BackgroundImage>>,
    pub ack: tokio::sync::oneshot::Sender<Result<(), String>>,
}

/// Encode a RawFrame to JPEG bytes, with optional rotation.
/// Kept for xvfb frame-dump preview path and tests.
pub fn encode_jpeg(frame: &RawFrame, quality: u8, rotation: u16) -> Result<Vec<u8>> {
    let (rotated, out_w, out_h) = rotate_pixels(&frame.data, frame.width, frame.height, rotation)?;
    crate::transport::encode::encode_jpeg_bytes(&rotated, out_w, out_h, quality)
}

struct PendingConnection {
    generation: u64,
    transport: Box<dyn Transport>,
    info: DeviceInfo,
    requested_at: Instant,
}

struct ActiveConnection {
    generation: u64,
    transport: Box<dyn Transport>,
    info: DeviceInfo,
}

/// Run the tick loop. Blocks until `shutdown` is signaled.
///
/// Connection lifecycle:
/// 1. `connector.connect()` success → `PendingConnection` + `SourceBuildRequest`
/// 2. Matching `SourceBuildResult::Ok` → apply background → `ActiveConnection`
///    then publish negotiated resolution + `connected=true` together
/// 3. Stale generation results are ignored; failed pending drops and retries after 2s
/// 4. Fatal send publishes `(0,0),false`, drops the generation, reconnects
/// Returns true when `desired` is the same artwork pointer that previously
/// failed to apply. Only `Some` artwork pointers are suppressed; a failed
/// clear-to-None is retried so the device can recover once the underlying
/// condition changes.
fn is_failed_artwork(
    failed: &Option<Arc<BackgroundImage>>,
    desired: &Option<Arc<BackgroundImage>>,
) -> bool {
    match (failed, desired) {
        (Some(failed_arc), Some(desired_arc)) => Arc::ptr_eq(failed_arc, desired_arc),
        _ => false,
    }
}

/// Apply the desired effective background to the frame source, but suppress
/// retrying the exact same pointer that previously failed. Clear the failure
/// memo whenever the desired value changes so that a config change (e.g.
/// disabling and re-enabling album art) always gets one fresh attempt.
fn apply_effective_background(
    frame_source: &mut Box<dyn FrameSource>,
    cached: &mut Option<Arc<BackgroundImage>>,
    failed: &mut Option<Arc<BackgroundImage>>,
    last_desired: &mut Option<Arc<BackgroundImage>>,
    effective: Option<Arc<BackgroundImage>>,
    context: &str,
) {
    if !mpris::backgrounds_equal(last_desired, &effective) {
        *last_desired = effective.clone();
        *failed = None;
    }
    if mpris::backgrounds_equal(cached, &effective) {
        return;
    }
    if is_failed_artwork(failed, &effective) {
        return;
    }
    match frame_source.set_background(effective.clone()) {
        Ok(()) => *cached = effective,
        Err(e) => {
            warn!("{context} failed: {e:#}");
            *failed = effective;
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_tick_loop(
    mut transport: Option<Box<dyn Transport>>,
    mut device_info: Option<DeviceInfo>,
    connector: TransportConnector,
    mut frame_source: Box<dyn FrameSource>,
    source_build_tx: tokio::sync::mpsc::Sender<SourceBuildRequest>,
    source_result_rx: &mut tokio::sync::mpsc::Receiver<SourceBuildResult>,
    sensor_hub: &mut SensorHub,
    tick_rate_fps: u32,
    jpeg_quality: u8,
    rotation: u16,
    mut template_rx: tokio::sync::watch::Receiver<String>,
    mut background_rx: tokio::sync::watch::Receiver<Option<Arc<BackgroundImage>>>,
    background_apply_rx: &mut tokio::sync::mpsc::Receiver<BackgroundApply>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    sensor_history: Option<Arc<Mutex<SensorHistory>>>,
    sensor_poll_interval: Duration,
    connected_tx: tokio::sync::watch::Sender<bool>,
    display_tx: tokio::sync::watch::Sender<RuntimeDisplayDimensions>,
    generation_tx: tokio::sync::watch::Sender<u64>,
    source_revision_rx: &mut tokio::sync::mpsc::Receiver<SourceRevisionApply>,
    mut tick_rate_rx: tokio::sync::watch::Receiver<u32>,
    media_snapshot: Arc<RwLock<MediaSnapshot>>,
    media_config_rx: tokio::sync::watch::Receiver<MediaConfig>,
) -> Result<()> {
    info!(
        "Tick loop started: {} FPS, JPEG quality={}, rotation={}°",
        tick_rate_fps, jpeg_quality, rotation
    );

    let mut generation: u64 = 0;
    let mut source_revision: u64 = 0;
    let mut pending: Option<PendingConnection> = None;
    let mut active: Option<ActiveConnection> = None;
    let mut cached_sensors: HashMap<String, String> = HashMap::new();
    let mut cached_background: Option<Arc<BackgroundImage>> = background_rx.borrow().clone();
    let mut cached_effective_background = mpris::effective_background(
        &media_config_rx.borrow(),
        &media_snapshot,
        &cached_background,
    );
    let mut last_desired_effective_background = cached_effective_background.clone();
    let mut failed_effective_background: Option<Arc<BackgroundImage>> = None;
    let mut last_poll = Instant::now() - sensor_poll_interval;
    let mut next_reconnect_at: Option<Instant> = None;

    // Startup: if we already have a transport+info, the caller built the source
    // at negotiated dimensions — commit ActiveConnection immediately.
    match (transport.take(), device_info.take()) {
        (Some(t), Some(info)) => {
            generation = 1;
            let _ = display_tx.send(RuntimeDisplayDimensions::new(info.width(), info.height()));
            let _ = connected_tx.send(true);
            let _ = generation_tx.send(generation);
            if let Some(bg) = &cached_effective_background {
                let _ = frame_source.set_background(Some(bg.clone()));
            }
            last_desired_effective_background = cached_effective_background.clone();
            active = Some(ActiveConnection {
                generation,
                transport: t,
                info,
            });
        }
        _ => {
            let _ = display_tx.send(RuntimeDisplayDimensions::new(0, 0));
            let _ = connected_tx.send(false);
            let _ = generation_tx.send(0);
            next_reconnect_at = Some(Instant::now());
        }
    }

    loop {
        let tick_start = Instant::now();
        let current_fps = (*tick_rate_rx.borrow_and_update()).max(1);
        let tick_duration = Duration::from_secs_f64(1.0 / f64::from(current_fps));

        while let Ok(apply) = source_revision_rx.try_recv() {
            if apply.revision < source_revision {
                let _ = apply.ack.send(Err(format!(
                    "stale source revision {} ignored (current {})",
                    apply.revision, source_revision
                )));
                continue;
            }

            source_revision = apply.revision;
            if apply.reset_connection {
                if let Some(mut connection) = pending.take() {
                    connection.transport.close();
                }
                if let Some(mut connection) = active.take() {
                    connection.transport.close();
                }
                let _ = display_tx.send(RuntimeDisplayDimensions::new(0, 0));
                let _ = connected_tx.send(false);
                let _ = generation_tx.send(0);
                next_reconnect_at = Some(Instant::now());
            } else if let Some(mut connection) = pending.take() {
                debug!("Invalidating pending source build at source revision {source_revision}");
                connection.transport.close();
                next_reconnect_at = Some(Instant::now());
            }
            let _ = apply.ack.send(Ok(()));
        }

        // Drain generation- and source-revision-tagged source builds.
        while let Ok(result) = source_result_rx.try_recv() {
            handle_source_result(
                result,
                &mut pending,
                &mut active,
                &mut frame_source,
                &cached_effective_background,
                &display_tx,
                &connected_tx,
                &generation_tx,
                &mut source_revision,
                &mut next_reconnect_at,
                &mut failed_effective_background,
            );
        }

        // Template updates apply to the current source (active or placeholder).
        if template_rx.has_changed().unwrap_or(false) {
            let template = template_rx.borrow_and_update().clone();
            if !template.is_empty() {
                frame_source.set_template(&template);
            }
        }

        // Transactional background applies (D-Bus set/clear). First validate the
        // requested user background; if it succeeds, ACK and best-effort restore
        // the effective (album-art) background. Only rollback when the user
        // background itself fails.
        while let Ok(apply) = background_apply_rx.try_recv() {
            let prior_cache = cached_background.clone();
            let prior_effective = cached_effective_background.clone();

            match frame_source.set_background(apply.image.clone()) {
                Ok(()) => {
                    cached_background = apply.image.clone();
                    // The renderer is now actually showing the user image.
                    cached_effective_background = apply.image.clone();
                    let _ = apply.ack.send(Ok(()));

                    // Restore album-art override if still active.
                    failed_effective_background = None;
                    last_desired_effective_background = cached_effective_background.clone();
                    let effective = mpris::effective_background(
                        &media_config_rx.borrow(),
                        &media_snapshot,
                        &cached_background,
                    );
                    apply_effective_background(
                        &mut frame_source,
                        &mut cached_effective_background,
                        &mut failed_effective_background,
                        &mut last_desired_effective_background,
                        effective,
                        "Album-art restore after background apply",
                    );
                }
                Err(e) => {
                    cached_background = prior_cache;
                    let _ = frame_source.set_background(prior_effective.clone());
                    cached_effective_background = prior_effective;
                    // Pending connection must abort on background raster failure.
                    if pending.is_some() {
                        warn!(
                            "Background apply failed during pending connection: {e:#} — aborting generation"
                        );
                        pending = None;
                        next_reconnect_at = Some(Instant::now() + Duration::from_secs(2));
                    }
                    let _ = apply.ack.send(Err(format!("{e:#}")));
                }
            }
        }
        // Watch channel mirrors for rebuilds (non-transactional hot path still ok
        // when no oneshot is involved, e.g. startup).
        if background_rx.has_changed().unwrap_or(false) {
            let bg = background_rx.borrow_and_update().clone();
            cached_background = bg;
            match frame_source.set_background(cached_background.clone()) {
                Ok(()) => {
                    cached_effective_background = cached_background.clone();
                    failed_effective_background = None;
                    last_desired_effective_background = cached_effective_background.clone();
                    let effective = mpris::effective_background(
                        &media_config_rx.borrow(),
                        &media_snapshot,
                        &cached_background,
                    );
                    apply_effective_background(
                        &mut frame_source,
                        &mut cached_effective_background,
                        &mut failed_effective_background,
                        &mut last_desired_effective_background,
                        effective,
                        "Background watch effective apply",
                    );
                }
                Err(e) => {
                    warn!("Background watch apply failed: {e:#}");
                }
            }
        }

        let effective = mpris::effective_background(
            &media_config_rx.borrow(),
            &media_snapshot,
            &cached_background,
        );
        apply_effective_background(
            &mut frame_source,
            &mut cached_effective_background,
            &mut failed_effective_background,
            &mut last_desired_effective_background,
            effective,
            "Effective background apply",
        );

        // Pending timeout: if source rebuild never arrives, drop and retry.
        if let Some(p) = &pending
            && p.requested_at.elapsed() > Duration::from_secs(5)
        {
            warn!(
                "Source rebuild for generation {} timed out — dropping pending connection",
                p.generation
            );
            pending = None;
            next_reconnect_at = Some(Instant::now() + Duration::from_secs(2));
        }

        // Discover when neither active nor pending.
        if active.is_none() && pending.is_none() {
            let due = next_reconnect_at
                .map(|t| Instant::now() >= t)
                .unwrap_or(true);
            if due {
                match tokio::task::block_in_place(|| connector.connect()) {
                    Ok((t, info)) => {
                        generation = generation.saturating_add(1);
                        info!(
                            "Device negotiated (pending gen {generation}): {}x{} PM={} SUB={} FBL={} {} {}",
                            info.width(),
                            info.height(),
                            info.pm,
                            info.sub,
                            info.fbl,
                            info.protocol,
                            info.encoding()
                        );
                        let (display_width, display_height) = info.oriented_dimensions(rotation)?;
                        let req = SourceBuildRequest {
                            generation,
                            width: display_width,
                            height: display_height,
                        };
                        if source_build_tx.try_send(req).is_err() {
                            warn!(
                                "Failed to request source rebuild for generation {generation}; retrying"
                            );
                            next_reconnect_at = Some(Instant::now() + Duration::from_secs(2));
                        } else {
                            pending = Some(PendingConnection {
                                generation,
                                transport: t,
                                info,
                                requested_at: Instant::now(),
                            });
                            next_reconnect_at = None;
                        }
                    }
                    Err(e) => {
                        debug!("Reconnect attempt failed: {e:#}");
                        next_reconnect_at = Some(Instant::now() + Duration::from_secs(2));
                    }
                }
            }
        }

        // Poll/render/encode are CPU-bound; run them under block_in_place so
        // Tokio can keep servicing D-Bus on other worker threads.
        let sensors = if tick_start.duration_since(last_poll) >= sensor_poll_interval {
            let data = tokio::task::block_in_place(|| sensor_hub.poll());
            if let Some(hist) = &sensor_history
                && let Ok(mut h) = hist.lock()
            {
                h.record(&data);
            }
            cached_sensors = data;
            last_poll = tick_start;
            &cached_sensors
        } else {
            &cached_sensors
        };

        // Render + encode + send only when ActiveConnection is committed.
        if let Some(conn) = active.as_mut() {
            let rendered = tokio::task::block_in_place(|| {
                let frame = frame_source.render(sensors)?;
                let encoded = encode_frame(&frame, &conn.info, rotation, jpeg_quality)?;
                Ok::<_, anyhow::Error>((frame, encoded))
            });
            match rendered {
                Ok((frame, encoded)) => {
                    debug!(
                        "Frame encoded: {} bytes ({})",
                        encoded.data.len(),
                        encoded.encoding
                    );

                    if frame_source.is_streaming() {
                        match frame_dump::frame_dir() {
                            Ok(dir) => {
                                let dump_bytes = if encoded.encoding.is_jpeg() {
                                    encoded.data.clone()
                                } else {
                                    encode_jpeg(&frame, jpeg_quality, 0).unwrap_or_default()
                                };
                                if !dump_bytes.is_empty()
                                    && let Err(e) = tokio::task::block_in_place(|| {
                                        frame_dump::write_frame_atomic(&dir, &dump_bytes)
                                    })
                                {
                                    warn!("frame_dump write failed: {e}");
                                }
                            }
                            Err(e) => warn!("frame_dump disabled: {e}"),
                        }
                    }

                    let send_result =
                        tokio::task::block_in_place(|| conn.transport.send_frame(&encoded));
                    if let Err(e) = send_result {
                        warn!("Failed to send frame: {e}");
                        if !conn.transport.is_connected() {
                            warn!(
                                "Fatal send — dropping generation {} and reconnecting",
                                conn.generation
                            );
                            let _ = display_tx.send(RuntimeDisplayDimensions::new(0, 0));
                            let _ = connected_tx.send(false);
                            let _ = generation_tx.send(0);
                            active = None;
                            pending = None;
                            next_reconnect_at = Some(Instant::now() + Duration::from_secs(2));
                        }
                    }
                }
                Err(e) => warn!("Render/encode failed: {e:#}"),
            }
        }

        let elapsed = tick_start.elapsed();
        if elapsed < tick_duration {
            tokio::time::sleep(tick_duration - elapsed).await;
        }

        if *shutdown.borrow_and_update() {
            break;
        }
    }
    if let Some(mut conn) = active.take() {
        conn.transport.close();
    }
    if let Some(mut p) = pending.take() {
        p.transport.close();
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn handle_source_result(
    result: SourceBuildResult,
    pending: &mut Option<PendingConnection>,
    active: &mut Option<ActiveConnection>,
    frame_source: &mut Box<dyn FrameSource>,
    cached_background: &Option<Arc<BackgroundImage>>,
    display_tx: &tokio::sync::watch::Sender<RuntimeDisplayDimensions>,
    connected_tx: &tokio::sync::watch::Sender<bool>,
    generation_tx: &tokio::sync::watch::Sender<u64>,
    current_source_revision: &mut u64,
    next_reconnect_at: &mut Option<Instant>,
    failed_effective_background: &mut Option<Arc<BackgroundImage>>,
) {
    let generation = result.generation;
    let source_revision = result.source_revision;
    let mut commit = result.commit;
    let pending_match = pending
        .as_ref()
        .is_some_and(|candidate| candidate.generation == generation);
    let active_match = active
        .as_ref()
        .is_some_and(|candidate| candidate.generation == generation);

    if source_revision < *current_source_revision || (!pending_match && !active_match) {
        debug!(
            "Ignoring stale SourceBuildResult for generation {generation}, revision {source_revision}"
        );
        if let Some(commit) = commit.take() {
            let _ = commit.send(Err(format!(
                "source generation {generation}, revision {source_revision} became stale before commit"
            )));
        }
        return;
    }

    match result.source {
        Ok(mut source) => {
            if let Some(background) = cached_background
                && let Err(error) = source.set_background(Some(background.clone()))
            {
                warn!("Failed to apply background to rebuilt source: {error:#}");
                if pending_match {
                    let _ = pending.take();
                    let _ = display_tx.send(RuntimeDisplayDimensions::new(0, 0));
                    let _ = connected_tx.send(false);
                    let _ = generation_tx.send(0);
                    *next_reconnect_at = Some(Instant::now() + Duration::from_secs(2));
                }
                if let Some(commit) = commit.take() {
                    let _ = commit.send(Err(format!(
                        "failed to apply background to source: {error:#}"
                    )));
                }
                return;
            }

            *current_source_revision = (*current_source_revision).max(source_revision);
            let was_streaming = frame_source.is_streaming();
            let is_streaming = source.is_streaming();
            if was_streaming && !is_streaming {
                match frame_dump::frame_dir() {
                    Ok(dir) => {
                        frame_dump::clear_frame_on_stream_exit(&dir, was_streaming, is_streaming)
                    }
                    Err(error) => {
                        warn!("Failed to locate stale stream frame for cleanup: {error:#}");
                    }
                }
            }

            *frame_source = source;
            // A new renderer is being committed; allow a previously failed
            // effective background one fresh attempt on the new source.
            *failed_effective_background = None;
            if pending_match {
                let connection = pending.take().expect("pending_match");
                info!(
                    "ActiveConnection generation {}: {}x{} {} {}",
                    connection.generation,
                    connection.info.width(),
                    connection.info.height(),
                    connection.info.protocol,
                    connection.info.encoding()
                );
                let _ = display_tx.send(RuntimeDisplayDimensions::new(
                    connection.info.width(),
                    connection.info.height(),
                ));
                let _ = connected_tx.send(true);
                let _ = generation_tx.send(connection.generation);
                *active = Some(ActiveConnection {
                    generation: connection.generation,
                    transport: connection.transport,
                    info: connection.info,
                });
                *next_reconnect_at = None;
            } else {
                info!(
                    "Frame source swapped for active generation {generation}, revision {source_revision}"
                );
            }
            if let Some(commit) = commit.take() {
                let _ = commit.send(Ok(()));
            }
        }
        Err(error) => {
            if pending_match {
                warn!(
                    "Source rebuild failed for pending generation {generation}: {error} — dropping and retrying"
                );
                *pending = None;
                let _ = display_tx.send(RuntimeDisplayDimensions::new(0, 0));
                let _ = connected_tx.send(false);
                let _ = generation_tx.send(0);
                *next_reconnect_at = Some(Instant::now() + Duration::from_secs(2));
            } else {
                warn!(
                    "Source rebuild failed for active generation {generation}: {error} — keeping prior source"
                );
            }
            if let Some(commit) = commit.take() {
                let _ = commit.send(Err(error));
            }
        }
    }
}

/// Helper for tests / examples constructing an EncodedFrame from JPEG bytes.
pub fn encoded_jpeg(data: Vec<u8>, width: u32, height: u32) -> EncodedFrame {
    EncodedFrame {
        data,
        width,
        height,
        encoding: crate::transport::FrameEncoding::Jpeg,
    }
}

#[cfg(test)]
mod tests {
    use super::encode_jpeg;
    use crate::render::RawFrame;
    use crate::transport::encode::encode_jpeg_bytes;

    #[test]
    fn encode_jpeg_delegates_to_encode_jpeg_bytes() {
        // 2x2 RGB: distinct corners so rotation + encode stay deterministic.
        let frame = RawFrame {
            data: vec![
                255, 0, 0, // (0,0) red
                0, 255, 0, // (1,0) green
                0, 0, 255, // (0,1) blue
                255, 255, 0, // (1,1) yellow
            ],
            width: 2,
            height: 2,
        };
        let quality = 90u8;

        let via_tick = encode_jpeg(&frame, quality, 0).expect("tick encode");
        let via_bytes = encode_jpeg_bytes(&frame.data, frame.width, frame.height, quality)
            .expect("bytes encode");
        assert_eq!(via_tick, via_bytes);

        let via_tick_rot = encode_jpeg(&frame, quality, 90).expect("tick rotate encode");
        let (rotated, w, h) =
            crate::transport::encode::rotate_pixels(&frame.data, frame.width, frame.height, 90)
                .expect("rotate");
        let via_bytes_rot =
            encode_jpeg_bytes(&rotated, w, h, quality).expect("bytes rotate encode");
        assert_eq!(via_tick_rot, via_bytes_rot);
    }
}
