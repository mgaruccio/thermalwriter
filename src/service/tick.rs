// SPDX-License-Identifier: GPL-3.0-or-later
//
// Tick loop: polls sensors, renders a frame, encodes, sends via transport.
// Connection generations and source revisions commit only after matching
// SourceBuildResults land.

use anyhow::Result;
use log::{debug, info, warn};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::render::background::BackgroundImage;
use crate::render::{FrameSource, RawFrame};
use crate::sensor::SensorHub;
use crate::sensor::history::SensorHistory;
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
    // Shared catalog updated after each poll so D-Bus list_sensors shows costs.
    sensor_catalog: Option<crate::service::SharedSensorCatalog>,
    connected_tx: tokio::sync::watch::Sender<bool>,
    display_tx: tokio::sync::watch::Sender<RuntimeDisplayDimensions>,
    generation_tx: tokio::sync::watch::Sender<u64>,
    source_revision_rx: &mut tokio::sync::mpsc::Receiver<SourceRevisionApply>,
    mut tick_rate_rx: tokio::sync::watch::Receiver<u32>,
    mut needed_rx: tokio::sync::watch::Receiver<Option<HashSet<String>>>,
    mut recipe_rx: tokio::sync::watch::Receiver<Option<crate::sensor::LayoutSensorRecipe>>,
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
    let mut last_poll = Instant::now() - sensor_poll_interval;
    let mut next_reconnect_at: Option<Instant> = None;
    // Dirty-frame skip: when the active source can fingerprint its inputs and
    // the fingerprint is unchanged, skip render/encode/send. LCD holds last
    // frame (no keepalive required). Invalidated on source/template/bg swap.
    let mut last_frame_fingerprint: Option<u64> = None;

    // Startup: if we already have a transport+info, the caller built the source
    // at negotiated dimensions — commit ActiveConnection immediately.
    match (transport.take(), device_info.take()) {
        (Some(t), Some(info)) => {
            generation = 1;
            let _ = display_tx.send(RuntimeDisplayDimensions::new(info.width(), info.height()));
            let _ = connected_tx.send(true);
            let _ = generation_tx.send(generation);
            if let Some(bg) = &cached_background {
                let _ = frame_source.set_background(Some(bg.clone()));
            }
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

        // Apply needed-keys updates from the mode listener / startup.
        if needed_rx.has_changed().unwrap_or(false) {
            let n = needed_rx.borrow_and_update().clone();
            sensor_hub.set_needed_keys(n);
        }

        // Fallback: if needed keys are still None (discovery mode) and the
        // catalog is now non-empty, compute from the active layout recipe.
        if sensor_hub.needed_keys().is_none() {
            if let Some(recipe) = recipe_rx.borrow_and_update().as_ref() {
                let known: HashSet<String> = sensor_hub
                    .available_sensors()
                    .into_iter()
                    .map(|d| d.key)
                    .collect();
                let declared: HashSet<String> = sensor_hub.declared_keys();
                if !known.is_empty() {
                    let frontmatter =
                        crate::render::frontmatter::LayoutFrontmatter::parse(&recipe.template);
                    let n = crate::sensor::layout_needed_keys(
                        &frontmatter,
                        &recipe.vars,
                        &recipe.template,
                        &known,
                        &declared,
                    );
                    if !n.is_empty() {
                        sensor_hub.set_needed_keys(Some(n));
                    }
                }
            }
        }

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
                &cached_background,
                &display_tx,
                &connected_tx,
                &generation_tx,
                &mut source_revision,
                &mut next_reconnect_at,
                &mut last_frame_fingerprint,
            );
        }

        // Template updates apply to the current source (active or placeholder).
        if template_rx.has_changed().unwrap_or(false) {
            let template = template_rx.borrow_and_update().clone();
            if !template.is_empty() {
                frame_source.set_template(&template);
                last_frame_fingerprint = None;
            }
        }

        // Transactional background applies (D-Bus set/clear). Ack only after
        // set_background succeeds; on failure keep prior cache/source state.
        while let Ok(apply) = background_apply_rx.try_recv() {
            let prior_cache = cached_background.clone();
            match frame_source.set_background(apply.image.clone()) {
                Ok(()) => {
                    cached_background = apply.image;
                    last_frame_fingerprint = None;
                    let _ = apply.ack.send(Ok(()));
                }
                Err(e) => {
                    // Restore prior source background if possible.
                    let _ = frame_source.set_background(prior_cache.clone());
                    cached_background = prior_cache;
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
            if let Err(e) = frame_source.set_background(bg.clone()) {
                warn!("Background watch apply failed: {e:#}");
            } else {
                cached_background = bg;
                last_frame_fingerprint = None;
            }
        }

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
        let sensors_refreshed = tick_start.duration_since(last_poll) >= sensor_poll_interval;
        let sensors = if sensors_refreshed {
            let data = tokio::task::block_in_place(|| sensor_hub.poll());
            if let Some(hist) = &sensor_history
                && let Ok(mut h) = hist.lock()
            {
                h.record(&data);
            }
            // Publish live costs for the GUI sensor picker.
            if let Some(catalog) = &sensor_catalog
                && let Ok(mut guard) = catalog.lock()
            {
                *guard = sensor_hub
                    .available_sensors()
                    .into_iter()
                    .map(|d| (d.key, d.name, d.unit, d.cost_us))
                    .collect();
            }
            cached_sensors = data;
            last_poll = tick_start;
            &cached_sensors
        } else {
            &cached_sensors
        };

        // Render + encode + send only when ActiveConnection is committed.
        if let Some(conn) = active.as_mut() {
            // Cheap dirty check: skip full render when inputs are unchanged.
            // Streaming sources return None and always render (Xvfb capture).
            // Compute fingerprint when sensors changed, source is time-varying,
            // or the cache was invalidated (None). Computing on invalidation
            // ensures one redraw rather than repeated redraws until the next
            // sensor poll (up to 2s with the 2000ms default).
            let fingerprint = if sensors_refreshed
                || frame_source.is_time_varying()
                || last_frame_fingerprint.is_none()
            {
                frame_source.content_fingerprint(sensors)
            } else {
                last_frame_fingerprint
            };
            let skip = matches!(
                (fingerprint, last_frame_fingerprint),
                (Some(now), Some(prev)) if now == prev
            );
            if skip {
                debug!("Skipping unchanged frame (fingerprint match)");
            } else {
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
                                last_frame_fingerprint = None;
                            }
                        } else {
                            last_frame_fingerprint = fingerprint;
                        }
                    }
                    Err(e) => warn!("Render/encode failed: {e:#}"),
                }
            }
        }

        let elapsed = tick_start.elapsed();
        if elapsed < tick_duration {
            // Sleep policy:
            // - Streaming or time-varying sources: sleep the remainder of tick_duration
            //   (need to render every frame).
            // - Non-time-varying: sleep until min(next_sensor_poll, tick_start + 250ms)
            //   so D-Bus/source channels still drain at ≥4 Hz without 2 Hz render wakeups.
            let time_varying = frame_source.is_time_varying();
            let sleep_dur = if frame_source.is_streaming() || time_varying {
                tick_duration - elapsed
            } else {
                let next_sensor = last_poll + sensor_poll_interval;
                let ceiling = tick_start + Duration::from_millis(250);
                let deadline = next_sensor.min(ceiling);
                let now = Instant::now();
                if deadline > now {
                    deadline - now
                } else {
                    Duration::from_millis(1)
                }
            };
            tokio::time::sleep(sleep_dur).await;
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
    last_frame_fingerprint: &mut Option<u64>,
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
            *last_frame_fingerprint = None;
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
