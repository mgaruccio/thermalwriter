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
use crate::transport::discovery::{ConnectedOutputs, OpenedDisplay, TransportConnector};
use crate::transport::encode::{adapt_frame_contain, encode_frame};
use crate::transport::{DeviceInfo, EncodedFrame};

pub use crate::transport::encode::rotate_pixels;

/// Request that the mode listener rebuild frame source(s) for a generation.
#[derive(Debug, Clone)]
pub struct SourceBuildRequest {
    pub generation: u64,
    /// Oriented canvas size per source to build.
    /// Mirror/single mode: one entry (primary). Independent: one per output.
    pub canvases: Vec<(u32, u32)>,
}

/// Result of a generation- and source-revision-tagged source rebuild (layout
/// swap or reconnect).
pub struct SourceBuildResult {
    pub generation: u64,
    pub source_revision: u64,
    pub sources: Result<Vec<Box<dyn FrameSource>>, String>,
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
    outputs: Vec<OpenedDisplay>,
    requested_at: Instant,
}

struct ActiveConnection {
    generation: u64,
    outputs: Vec<OpenedDisplay>,
}

fn close_outputs(outputs: &mut [OpenedDisplay]) {
    for output in outputs.iter_mut() {
        output.transport.close();
    }
}

fn try_connect(connector: &TransportConnector) -> Result<ConnectedOutputs> {
    connector.connect_all()
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
    mut initial_outputs: Option<Vec<OpenedDisplay>>,
    mut primary_info: Option<DeviceInfo>,
    connector: TransportConnector,
    mut frame_sources: Vec<Box<dyn FrameSource>>,
    // When true, one source per output with per-output rotation (no letterbox).
    independent: bool,
    source_build_tx: tokio::sync::mpsc::Sender<SourceBuildRequest>,
    source_result_rx: &mut tokio::sync::mpsc::Receiver<SourceBuildResult>,
    sensor_hub: &mut SensorHub,
    tick_rate_fps: u32,
    jpeg_quality: u8,
    // Per-output rotations when independent; otherwise exactly one shared value.
    rotations: Vec<u16>,
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
    display_count_tx: tokio::sync::watch::Sender<u32>,
    generation_tx: tokio::sync::watch::Sender<u64>,
    source_revision_rx: &mut tokio::sync::mpsc::Receiver<SourceRevisionApply>,
    mut tick_rate_rx: tokio::sync::watch::Receiver<u32>,
    mut needed_rx: tokio::sync::watch::Receiver<Option<HashSet<String>>>,
    mut recipe_rx: tokio::sync::watch::Receiver<Option<crate::sensor::LayoutSensorRecipe>>,
) -> Result<()> {
    let shared_rotation = rotations.first().copied().unwrap_or(0);
    info!(
        "Tick loop started: {} FPS, JPEG quality={}, independent={}, rotations={:?}",
        tick_rate_fps, jpeg_quality, independent, rotations
    );

    if frame_sources.is_empty() {
        anyhow::bail!("tick loop requires at least one frame source");
    }
    if independent {
        if rotations.len() != frame_sources.len() {
            anyhow::bail!(
                "independent mode requires rotations.len() == sources.len() ({} vs {})",
                rotations.len(),
                frame_sources.len()
            );
        }
    } else if rotations.len() != 1 {
        anyhow::bail!("mirror/single mode requires exactly one rotation entry");
    }

    let mut generation: u64 = 0;
    let mut source_revision: u64 = 0;
    let mut pending: Option<PendingConnection> = None;
    let mut active: Option<ActiveConnection> = None;
    let mut cached_sensors: HashMap<String, String> = HashMap::new();
    let mut cached_background: Option<Arc<BackgroundImage>> = background_rx.borrow().clone();
    let mut last_poll = Instant::now() - sensor_poll_interval;
    let mut next_reconnect_at: Option<Instant> = None;
    // Dirty-frame skip per source. LCD holds last frame (no keepalive required).
    let mut last_frame_fingerprints: Vec<Option<u64>> = vec![None; frame_sources.len()];

    // Startup: if we already have outputs+primary, the caller built sources
    // at oriented dimensions — commit ActiveConnection immediately.
    match (initial_outputs.take(), primary_info.take()) {
        (Some(outputs), Some(info)) if !outputs.is_empty() => {
            generation = 1;
            let count = outputs.len() as u32;
            let _ = display_tx.send(RuntimeDisplayDimensions::new(info.width(), info.height()));
            let _ = display_count_tx.send(count);
            let _ = connected_tx.send(true);
            let _ = generation_tx.send(generation);
            if let Some(bg) = &cached_background {
                for source in &mut frame_sources {
                    let _ = source.set_background(Some(bg.clone()));
                }
            }
            active = Some(ActiveConnection {
                generation,
                outputs,
            });
        }
        _ => {
            let _ = display_tx.send(RuntimeDisplayDimensions::new(0, 0));
            let _ = display_count_tx.send(0);
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
                    close_outputs(&mut connection.outputs);
                }
                if let Some(mut connection) = active.take() {
                    close_outputs(&mut connection.outputs);
                }
                let _ = display_tx.send(RuntimeDisplayDimensions::new(0, 0));
                let _ = display_count_tx.send(0);
                let _ = connected_tx.send(false);
                let _ = generation_tx.send(0);
                next_reconnect_at = Some(Instant::now());
            } else if let Some(mut connection) = pending.take() {
                debug!("Invalidating pending source build at source revision {source_revision}");
                close_outputs(&mut connection.outputs);
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
                &mut frame_sources,
                &cached_background,
                &display_tx,
                &display_count_tx,
                &connected_tx,
                &generation_tx,
                &mut source_revision,
                &mut next_reconnect_at,
                &mut last_frame_fingerprints,
            );
        }

        // Template updates apply to the current source (active or placeholder).
        if template_rx.has_changed().unwrap_or(false) {
            let template = template_rx.borrow_and_update().clone();
            if !template.is_empty() {
                // Primary-only template hot-reload (D-Bus layout path).
                frame_sources[0].set_template(&template);
                last_frame_fingerprints[0] = None;
            }
        }

        // Transactional background applies (D-Bus set/clear). Ack only after
        // set_background succeeds; on failure keep prior cache/source state.
        while let Ok(apply) = background_apply_rx.try_recv() {
            let prior_cache = cached_background.clone();
            let mut apply_err = None;
            for source in &mut frame_sources {
                if let Err(e) = source.set_background(apply.image.clone()) {
                    apply_err = Some(e);
                    break;
                }
            }
            match apply_err {
                None => {
                    cached_background = apply.image;
                    last_frame_fingerprints.fill(None);
                    let _ = apply.ack.send(Ok(()));
                }
                Some(e) => {
                    // Restore prior source background if possible.
                    for source in &mut frame_sources {
                        let _ = source.set_background(prior_cache.clone());
                    }
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
            let mut err = None;
            for source in &mut frame_sources {
                if let Err(e) = source.set_background(bg.clone()) {
                    err = Some(e);
                    break;
                }
            }
            if let Some(e) = err {
                warn!("Background watch apply failed: {e:#}");
            } else {
                cached_background = bg;
                last_frame_fingerprints.fill(None);
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
                match tokio::task::block_in_place(|| try_connect(&connector)) {
                    Ok(connected) => {
                        generation = generation.saturating_add(1);
                        let primary = connected.primary().clone();
                        let count = connected.display_count();
                        info!(
                            "Device negotiated (pending gen {generation}, {count} display(s)): {}x{} PM={} SUB={} FBL={} {} {}",
                            primary.width(),
                            primary.height(),
                            primary.pm,
                            primary.sub,
                            primary.fbl,
                            primary.protocol,
                            primary.encoding()
                        );
                        let canvases = if independent {
                            let mut list = Vec::with_capacity(connected.outputs.len());
                            for (idx, output) in connected.outputs.iter().enumerate() {
                                let rot = rotations.get(idx).copied().unwrap_or(shared_rotation);
                                list.push(output.info.oriented_dimensions(rot)?);
                            }
                            list
                        } else {
                            vec![primary.oriented_dimensions(shared_rotation)?]
                        };
                        let req = SourceBuildRequest {
                            generation,
                            canvases,
                        };
                        if source_build_tx.try_send(req).is_err() {
                            warn!(
                                "Failed to request source rebuild for generation {generation}; retrying"
                            );
                            for mut output in connected.outputs {
                                output.transport.close();
                            }
                            next_reconnect_at = Some(Instant::now() + Duration::from_secs(2));
                        } else {
                            pending = Some(PendingConnection {
                                generation,
                                outputs: connected.outputs,
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
            // Ensure fingerprint slots track source count after rebuilds.
            if last_frame_fingerprints.len() != frame_sources.len() {
                last_frame_fingerprints.resize(frame_sources.len(), None);
            }

            let rendered = tokio::task::block_in_place(|| {
                let mut fatal_disconnect = false;
                let mut send_error = None;
                let mut dump_frame = None;

                if independent {
                    if frame_sources.len() != conn.outputs.len() {
                        return Err((
                            false,
                            anyhow::anyhow!(
                                "independent source/output count mismatch: {} vs {}",
                                frame_sources.len(),
                                conn.outputs.len()
                            ),
                        ));
                    }
                    for (idx, output) in conn.outputs.iter_mut().enumerate() {
                        let source = &mut frame_sources[idx];
                        let rotation = rotations[idx];
                        let fingerprint = if sensors_refreshed
                            || source.is_time_varying()
                            || last_frame_fingerprints[idx].is_none()
                        {
                            source.content_fingerprint(sensors)
                        } else {
                            last_frame_fingerprints[idx]
                        };
                        let skip = matches!(
                            (fingerprint, last_frame_fingerprints[idx]),
                            (Some(now), Some(prev)) if now == prev
                        );
                        if skip {
                            continue;
                        }
                        let frame = match source.render(sensors) {
                            Ok(frame) => frame,
                            Err(error) => return Err((false, error)),
                        };
                        let encoded =
                            match encode_frame(&frame, &output.info, rotation, jpeg_quality) {
                                Ok(encoded) => encoded,
                                Err(error) => return Err((false, error)),
                            };
                        if let Err(error) = output.transport.send_frame(&encoded) {
                            if !output.transport.is_connected() {
                                fatal_disconnect = true;
                            }
                            send_error = Some(error);
                            break;
                        }
                        last_frame_fingerprints[idx] = fingerprint;
                        if idx == 0 {
                            dump_frame = Some((frame, rotation, source.is_streaming()));
                        }
                    }
                } else {
                    let source = &mut frame_sources[0];
                    let rotation = shared_rotation;
                    let fingerprint = if sensors_refreshed
                        || source.is_time_varying()
                        || last_frame_fingerprints[0].is_none()
                    {
                        source.content_fingerprint(sensors)
                    } else {
                        last_frame_fingerprints[0]
                    };
                    let skip = matches!(
                        (fingerprint, last_frame_fingerprints[0]),
                        (Some(now), Some(prev)) if now == prev
                    );
                    if !skip {
                        let frame = match source.render(sensors) {
                            Ok(frame) => frame,
                            Err(error) => return Err((false, error)),
                        };
                        for output in &mut conn.outputs {
                            let (target_w, target_h) =
                                match output.info.oriented_dimensions(rotation) {
                                    Ok(dimensions) => dimensions,
                                    Err(error) => return Err((false, error)),
                                };
                            let adapted = match adapt_frame_contain(&frame, target_w, target_h) {
                                Ok(frame) => frame,
                                Err(error) => return Err((false, error)),
                            };
                            let encoded = match encode_frame(
                                &adapted,
                                &output.info,
                                rotation,
                                jpeg_quality,
                            ) {
                                Ok(encoded) => encoded,
                                Err(error) => return Err((false, error)),
                            };
                            if let Err(error) = output.transport.send_frame(&encoded) {
                                if !output.transport.is_connected() {
                                    fatal_disconnect = true;
                                }
                                send_error = Some(error);
                                break;
                            }
                        }
                        last_frame_fingerprints[0] = fingerprint;
                        dump_frame = Some((frame, rotation, source.is_streaming()));
                    }
                }

                if let Some(error) = send_error {
                    return Err((fatal_disconnect, error));
                }
                Ok(dump_frame)
            });
            match rendered {
                Ok(dump_frame) => {
                    if let Some((frame, rotation, streaming)) = dump_frame {
                        debug!("Frame rendered for {} display(s)", conn.outputs.len());
                        if streaming {
                            match frame_dump::frame_dir() {
                                Ok(dir) => {
                                    let dump_bytes = encode_jpeg(&frame, jpeg_quality, rotation)
                                        .unwrap_or_default();
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
                    }
                }
                Err((fatal_disconnect, error)) => {
                    if fatal_disconnect {
                        warn!("Failed to send frame: {error:#}");
                        warn!(
                            "Fatal send — dropping generation {} and reconnecting display group",
                            conn.generation
                        );
                        close_outputs(&mut conn.outputs);
                        let _ = display_tx.send(RuntimeDisplayDimensions::new(0, 0));
                        let _ = display_count_tx.send(0);
                        let _ = connected_tx.send(false);
                        let _ = generation_tx.send(0);
                        active = None;
                        pending = None;
                        next_reconnect_at = Some(Instant::now() + Duration::from_secs(2));
                        last_frame_fingerprints.fill(None);
                    } else {
                        warn!("Render/encode/send failed: {error:#}");
                    }
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
            let time_varying = frame_sources.iter().any(|s| s.is_time_varying());
            let streaming = frame_sources.iter().any(|s| s.is_streaming());
            let sleep_dur = if streaming || time_varying {
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
        close_outputs(&mut conn.outputs);
    }
    if let Some(mut pending_conn) = pending.take() {
        close_outputs(&mut pending_conn.outputs);
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn handle_source_result(
    result: SourceBuildResult,
    pending: &mut Option<PendingConnection>,
    active: &mut Option<ActiveConnection>,
    frame_sources: &mut Vec<Box<dyn FrameSource>>,
    cached_background: &Option<Arc<BackgroundImage>>,
    display_tx: &tokio::sync::watch::Sender<RuntimeDisplayDimensions>,
    display_count_tx: &tokio::sync::watch::Sender<u32>,
    connected_tx: &tokio::sync::watch::Sender<bool>,
    generation_tx: &tokio::sync::watch::Sender<u64>,
    current_source_revision: &mut u64,
    next_reconnect_at: &mut Option<Instant>,
    last_frame_fingerprints: &mut Vec<Option<u64>>,
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

    match result.sources {
        Ok(mut sources) => {
            if sources.is_empty() {
                if let Some(commit) = commit.take() {
                    let _ = commit.send(Err("source rebuild returned zero sources".into()));
                }
                return;
            }
            if let Some(background) = cached_background {
                for source in &mut sources {
                    if let Err(error) = source.set_background(Some(background.clone())) {
                        warn!("Failed to apply background to rebuilt source: {error:#}");
                        if pending_match {
                            let _ = pending.take();
                            let _ = display_tx.send(RuntimeDisplayDimensions::new(0, 0));
                            let _ = display_count_tx.send(0);
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
                }
            }

            *current_source_revision = (*current_source_revision).max(source_revision);
            let was_streaming = frame_sources.iter().any(|s| s.is_streaming());
            let is_streaming = sources.iter().any(|s| s.is_streaming());
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
            // Primary-only D-Bus swap: single rebuilt source into multi-source set.
            if !pending_match && sources.len() == 1 && frame_sources.len() > 1 {
                frame_sources[0] = sources.into_iter().next().expect("len checked");
            } else {
                *frame_sources = sources;
            }
            last_frame_fingerprints.clear();
            last_frame_fingerprints.resize(frame_sources.len(), None);
            if pending_match {
                let connection = pending.take().expect("pending_match");
                let primary = connection
                    .outputs
                    .first()
                    .map(|output| &output.info)
                    .expect("pending connection must have outputs");
                info!(
                    "ActiveConnection generation {}: {} display(s), primary {}x{} {} {}",
                    connection.generation,
                    connection.outputs.len(),
                    primary.width(),
                    primary.height(),
                    primary.protocol,
                    primary.encoding()
                );
                let _ = display_tx.send(RuntimeDisplayDimensions::new(
                    primary.width(),
                    primary.height(),
                ));
                let _ = display_count_tx.send(connection.outputs.len() as u32);
                let _ = connected_tx.send(true);
                let _ = generation_tx.send(connection.generation);
                *active = Some(ActiveConnection {
                    generation: connection.generation,
                    outputs: connection.outputs,
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
                let _ = display_count_tx.send(0);
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
