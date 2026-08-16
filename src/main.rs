use anyhow::Result;
use clap::Parser;
use log::{info, warn};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc, watch};

use thermalwriter::cli::{Cli, Command};
use thermalwriter::config::{Config, builtin_layouts};
use thermalwriter::render::TemplateRenderer;
use thermalwriter::render::background::BackgroundImage;
use thermalwriter::render::frontmatter::LayoutFrontmatter;
use thermalwriter::render::svg::SvgRenderer;
use thermalwriter::render::{FrameSource, background as bg_decode};
use thermalwriter::sensor::SensorHub;
use thermalwriter::sensor::history::SensorHistory;
use thermalwriter::sensor::layout_needed_keys;
use thermalwriter::service::dbus::{self, ModeChange, ServiceState};
use thermalwriter::service::mode_handler::RuntimeDisplayDimensions;
use thermalwriter::service::tick::{
    self, BackgroundApply, SourceBuildRequest, SourceBuildResult, SourceRevisionApply,
};
use thermalwriter::theme::ThemePalette;
use thermalwriter::transport::DeviceInfo;
use thermalwriter::transport::discovery::{DeviceSelector, OpenedDisplay, TransportConnector};

// Heap allocation profiling, behind the `dhat-heap` feature (never enabled in
// the shipped default build). The global allocator swap applies to the whole
// process, but the `Profiler` below is only created for actual daemon runs —
// it writes its output file (dhat-heap.json) when dropped, which happens on
// any exit from `main` (clean SIGTERM shutdown or an early startup error).
#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

async fn commit_frame_source(
    sender: &mpsc::Sender<SourceBuildResult>,
    generation: u64,
    source_revision: u64,
    sources: Vec<Box<dyn FrameSource>>,
) -> Result<()> {
    let (commit_tx, commit_rx) = tokio::sync::oneshot::channel();
    sender
        .send(SourceBuildResult {
            generation,
            source_revision,
            sources: Ok(sources),
            commit: Some(commit_tx),
        })
        .await
        .map_err(|_| anyhow::anyhow!("tick loop dropped source result channel"))?;
    match commit_rx.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(anyhow::anyhow!(error)),
        Err(_) => Err(anyhow::anyhow!(
            "tick loop dropped source commit acknowledgement"
        )),
    }
}

async fn apply_source_revision(
    sender: &mpsc::Sender<SourceRevisionApply>,
    revision: u64,
    reset_connection: bool,
) -> Result<()> {
    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    sender
        .send(SourceRevisionApply {
            revision,
            reset_connection,
            ack: ack_tx,
        })
        .await
        .map_err(|_| anyhow::anyhow!("tick loop dropped source revision channel"))?;
    match ack_rx.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(anyhow::anyhow!(error)),
        Err(_) => Err(anyhow::anyhow!(
            "tick loop dropped source revision acknowledgement"
        )),
    }
}
/// Compute layout-derived needed keys from a layout file and send them on the
/// watch channel. Reads the sensor catalog (shared, updated by the tick loop
/// after each poll) to resolve known keys. If the catalog is empty (pre-discovery),
/// sends `None` so the tick loop falls back to full discovery.
fn send_layout_needed_keys(
    layout_path: &std::path::Path,
    vars: &HashMap<String, String>,
    sensor_descriptors: &thermalwriter::service::SharedSensorCatalog,
    declared_keys: &HashSet<String>,
    needed_tx: &tokio::sync::watch::Sender<Option<HashSet<String>>>,
    recipe_tx: &tokio::sync::watch::Sender<Option<thermalwriter::sensor::LayoutSensorRecipe>>,
) {
    let Ok(template) = std::fs::read_to_string(layout_path) else {
        return;
    };
    let frontmatter = thermalwriter::render::frontmatter::LayoutFrontmatter::parse(&template);
    let known: HashSet<String> = sensor_descriptors
        .lock()
        .map(|guard| guard.iter().map(|(k, _, _, _)| k.clone()).collect())
        .unwrap_or_default();
    if known.is_empty() {
        // Pre-discovery: send None for full discovery, but still record the recipe.
        let _ = recipe_tx.send(Some(thermalwriter::sensor::LayoutSensorRecipe {
            template: template.clone(),
            vars: vars.clone(),
        }));
        let _ = needed_tx.send(None);
        return;
    }
    let needed = thermalwriter::sensor::layout_needed_keys(
        &frontmatter,
        vars,
        &template,
        &known,
        declared_keys,
    );
    let _ = recipe_tx.send(Some(thermalwriter::sensor::LayoutSensorRecipe {
        template,
        vars: vars.clone(),
    }));
    if !needed.is_empty() {
        let _ = needed_tx.send(Some(needed));
    } else {
        let _ = needed_tx.send(None);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    let cli = Cli::parse();
    match cli.command {
        Command::Bench { duration } => {
            return thermalwriter::cli::run_bench(duration);
        }
        Command::Ctl { subcommand } => {
            return thermalwriter::cli::run_ctl(subcommand).await;
        }
        Command::SetupUdev => {
            return thermalwriter::cli::run_setup_udev();
        }
        Command::ValidateDevice {
            device,
            bus_address,
            passive,
            output,
        } => {
            return thermalwriter::cli::run_validate_device_cmd(
                thermalwriter::transport::ValidateDeviceArgs {
                    device,
                    bus_address,
                    passive,
                    output,
                },
            );
        }
        Command::Daemon => {} // fall through to daemon startup below
    }

    #[cfg(feature = "dhat-heap")]
    let _profiler = dhat::Profiler::new_heap();

    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()))
        .join("thermalwriter");
    let layout_dir = config_dir.join("layouts");
    std::fs::create_dir_all(&layout_dir)?;

    // Load config (defaults if file missing, error if invalid TOML)
    let config_path = config_dir.join("config.toml");
    let config = Config::load(&config_path)?;
    info!(
        "Config: tick_rate={}, layout={}, jpeg_quality={}, mode={}",
        config.display.tick_rate,
        config.display.default_layout,
        config.display.jpeg_quality,
        config.display.mode
    );

    // Seed built-in layouts on first run (only if files don't exist)
    builtin_layouts::seed_layout_dir(&layout_dir)?;

    // Seed built-in background images on first run
    let background_dir = config_dir.join("backgrounds");
    std::fs::create_dir_all(&background_dir)?;
    builtin_layouts::seed_background_dir(&background_dir)?;

    // Seed built-in Xvfb wrapper configs (conky + cava) on first run
    let wrapper_dir = config_dir.join("wrappers");
    builtin_layouts::seed_wrapper_dir(&wrapper_dir)?;

    // Decode the configured background image at startup (if set)
    let initial_background: Option<Arc<BackgroundImage>> =
        if let Some(image_name) = &config.background.image {
            let bg_path = background_dir.join(image_name);
            match bg_decode::load_background(&bg_path) {
                Ok(img) => Some(Arc::new(img)),
                Err(e) => {
                    log::warn!("Failed to decode background '{}': {}", image_name, e);
                    None
                }
            }
        } else {
            None
        };

    // Setup transport via connector. Absence of the display at daemon startup must
    // not prevent D-Bus from coming up; the tick loop rediscovers until it appears.
    // THERMALWRITER_TRANSPORT=null / THERMALWRITER_PROFILE select headless fixtures.
    let independent_outputs = if config.has_independent_displays() {
        Some(config.resolved_display_outputs()?)
    } else {
        None
    };
    let independent = independent_outputs.is_some();
    let connector = if let Some(ref outputs) = independent_outputs {
        let targets: Vec<(u16, u16)> = outputs.iter().map(|o| o.usb_id()).collect();
        info!(
            "Independent multi-display: {} target(s): {}",
            targets.len(),
            outputs
                .iter()
                .map(|o| o.identity())
                .collect::<Vec<_>>()
                .join(", ")
        );
        TransportConnector::with_targets(targets)
    } else {
        TransportConnector::from_config_device(&config.display.device).unwrap_or_else(|e| {
            warn!(
                "Invalid display.device={:?}: {e:#}; falling back to auto",
                config.display.device
            );
            TransportConnector::new(DeviceSelector::Auto)
        })
    };
    let (initial_outputs, device_info, connected, display, display_count): (
        Option<Vec<OpenedDisplay>>,
        Option<DeviceInfo>,
        bool,
        RuntimeDisplayDimensions,
        u32,
    ) = match connector.connect_all() {
        Ok(connected) => {
            let primary = connected.primary().clone();
            let count = connected.display_count();
            info!(
                "Device: {} display(s), primary {}x{}, PM={}, SUB={}, FBL={}, protocol={}, encoding={}",
                count,
                primary.width(),
                primary.height(),
                primary.pm,
                primary.sub,
                primary.fbl,
                primary.protocol,
                primary.encoding()
            );
            (
                Some(connected.outputs),
                Some(primary.clone()),
                true,
                RuntimeDisplayDimensions::new(primary.width(), primary.height()),
                count,
            )
        }
        Err(e) => {
            warn!("Display unavailable at startup: {e:#}; daemon will keep running and retry");
            // Disconnected publishes (0,0); 480×480 is internal-only for layout authoring.
            (None, None, false, RuntimeDisplayDimensions::new(0, 0), 0)
        }
    };

    const INTERNAL_CANVAS_W: u32 = 480;
    const INTERNAL_CANVAS_H: u32 = 480;
    // Internal authoring canvas when disconnected; never published on D-Bus as connected dims.
    let primary_rotation = independent_outputs
        .as_ref()
        .and_then(|o| o.first().map(|x| x.rotation))
        .unwrap_or(config.display.rotation);
    let source_display = if let Some(info) = device_info.as_ref() {
        let (width, height) = info.oriented_dimensions(primary_rotation)?;
        RuntimeDisplayDimensions::new(width, height)
    } else {
        RuntimeDisplayDimensions::new(INTERNAL_CANVAS_W, INTERNAL_CANVAS_H)
    };

    // Setup sensor hub with all providers
    let mut sensor_hub = SensorHub::with_default_providers(&config.sensors.mangohud_log_dir);

    // Prime providers so they discover devices, then publish the live catalog
    // for D-Bus list_sensors (includes per-key poll cost after the first poll).
    let _ = sensor_hub.poll();
    let sensor_descriptors = Arc::new(std::sync::Mutex::new(
        sensor_hub
            .available_sensors()
            .into_iter()
            .map(|d| (d.key, d.name, d.unit, d.cost_us))
            .collect::<Vec<_>>(),
    ));
    // Canonical keys declared by providers (regardless of current readability).
    // Shared with the mode listener so layout-needed-key derivation can include
    // transiently-unreadable sensors (e.g. RAPL's cpu_power).
    let declared_keys = Arc::new(sensor_hub.declared_keys());

    // Generation-tagged source rebuild channel (tick → listener request, listener → tick result)
    let (source_build_tx, mut source_build_rx) = mpsc::channel::<SourceBuildRequest>(4);
    let (source_result_tx, mut source_result_rx) = mpsc::channel::<SourceBuildResult>(4);
    let (source_revision_tx, mut source_revision_rx) = mpsc::channel::<SourceRevisionApply>(4);
    let (background_apply_tx, mut background_apply_rx) = mpsc::channel::<BackgroundApply>(4);
    // Active connection generation (0 = disconnected). Layout/mode swaps tag results with this.
    let (generation_tx, generation_rx) = watch::channel::<u64>(0);
    // Shared shutdown + template channels
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (connected_tx, _) = watch::channel(connected);
    let (display_tx, _) = watch::channel(display);
    let (display_count_tx, _) = watch::channel(display_count);
    let (template_tx, template_rx) = watch::channel(String::new());
    // Background watch channel: mode-change listener → tick loop (immediate apply)
    let (background_tx, background_rx) =
        watch::channel::<Option<Arc<BackgroundImage>>>(initial_background.clone());
    // Tick rate watch channel: D-Bus set_tick_rate → tick loop (no restart needed)
    let (tick_rate_tx, tick_rate_rx) = watch::channel::<u32>(config.display.tick_rate);
    // Needed-keys watch channel: main.rs / mode listener → tick loop.
    // None = full discovery (pre-catalog or xvfb). Some(set) = prune to layout needs.
    let (needed_tx, needed_rx) = watch::channel::<Option<HashSet<String>>>(None);
    // Active layout recipe: lets the tick loop recompute needed keys when the
    // catalog transitions from empty to non-empty.
    let (recipe_tx, recipe_rx) =
        watch::channel::<Option<thermalwriter::sensor::LayoutSensorRecipe>>(None);
    // Mode change channel (D-Bus → listener task)
    let (mode_tx, mut mode_rx) = mpsc::channel::<ModeChange>(4);

    // Determine initial frame source, tick rate, and sensor history based on config mode
    let xvfb_tick_rate = config.xvfb.tick_rate.clamp(1, 60);
    // Always allocate SensorHistory so a later D-Bus set_layout into a history-using layout
    // populates correctly even when starting in xvfb mode.
    let initial_sensor_history: Option<Arc<std::sync::Mutex<SensorHistory>>> =
        Some(Arc::new(std::sync::Mutex::new(SensorHistory::new())));
    let (initial_frame_source, initial_xvfb_handle, active_tick_rate, resolved_active_layout) =
        if !independent && config.display.mode == "xvfb" {
            if config.xvfb.command.is_empty() {
                anyhow::bail!("xvfb mode requires [xvfb] command in config");
            }
            let (handle, source) = source_display.start_xvfb_shell(&config.xvfb.command)?;
            let boxed: Box<dyn FrameSource> = Box::new(source);
            // Xvfb mode: sensors are unused for frame content — skip all
            // optional provider work by sending an empty needed set.
            let _ = needed_tx.send(Some(HashSet::new()));
            // History is allocated but not seeded with layout frontmatter — metrics get configured
            // when the user switches to a layout via D-Bus set_layout.
            (
                boxed,
                Some(handle),
                xvfb_tick_rate,
                config.display.default_layout.clone(),
            )
        } else {
            // Load configured layout — user file takes precedence over built-in.
            // Missing files fall back to a builtin whose content AND identity match
            // the configured layout kind (SVG↔SVG, HTML↔HTML).
            let layout_path = layout_dir.join(&config.display.default_layout);
            let on_disk = if layout_path.exists() {
                Some(std::fs::read_to_string(&layout_path)?)
            } else {
                None
            };
            let (resolved_layout, template) =
                builtin_layouts::resolve_layout_identity(&config.display.default_layout, on_disk);

            let frontmatter = LayoutFrontmatter::parse(&template);
            // Configure history metrics from the layout's frontmatter into the
            // already-allocated shared SensorHistory.
            if let Some(ref hist) = initial_sensor_history
                && let Ok(mut h) = hist.lock()
            {
                for (metric, cfg) in &frontmatter.history_configs {
                    h.configure_metric(metric, cfg.duration);
                }
            }

            let theme_palette: ThemePalette = config
                .theme
                .resolve_palette()
                .map_err(|e| anyhow::anyhow!("invalid theme configuration: {e}"))?;
            let layout_vars = config
                .layout_vars
                .get(&resolved_layout)
                .cloned()
                .unwrap_or_default();

            // Adaptive prune: only spend poll budget on metrics this layout
            // actually displays. Derive from frontmatter + template tokens
            // against the sensor catalog (built from the initial poll above).
            let known: HashSet<String> = sensor_hub
                .available_sensors()
                .into_iter()
                .map(|d| d.key)
                .collect();
            let declared: HashSet<String> = sensor_hub.declared_keys();
            let needed =
                layout_needed_keys(&frontmatter, &layout_vars, &template, &known, &declared);
            if !needed.is_empty() {
                let _ = needed_tx.send(Some(needed));
            }
            // Send the recipe so the tick loop can recompute if the catalog
            // transitions from empty to non-empty.
            let _ = recipe_tx.send(Some(thermalwriter::sensor::LayoutSensorRecipe {
                template: template.clone(),
                vars: layout_vars.clone(),
            }));

            let is_layout_document = resolved_layout.ends_with(".layout.toml");
            let is_svg = resolved_layout.ends_with(".svg");
            let boxed: Box<dyn FrameSource> = if is_layout_document {
                source_display.build_layout_source_with_bindings(
                    &layout_dir.join(&resolved_layout),
                    HashMap::new(),
                    None,
                    None,
                    theme_palette.clone(),
                    &declared_keys,
                )?
            } else if is_svg {
                let mut renderer =
                    SvgRenderer::new(&template, source_display.width(), source_display.height())?;
                if let Some(ref hist) = initial_sensor_history {
                    renderer.set_history(hist.clone());
                }
                renderer.set_theme(theme_palette);
                renderer.set_layout_vars(layout_vars);
                let _ = renderer.set_background(initial_background.clone());
                #[cfg(feature = "video")]
                if let Some(video_cfg) = &config.background.video {
                    match thermalwriter::render::video::VideoFit::parse(&video_cfg.fit).and_then(
                        |fit| renderer.set_video_background(&video_cfg.path, video_cfg.fps, fit),
                    ) {
                        Ok(()) => log::info!(
                            "video background started: {} @ {} fps ({})",
                            video_cfg.path,
                            video_cfg.fps,
                            video_cfg.fit
                        ),
                        Err(e) => {
                            log::warn!("video background '{}' not started: {}", video_cfg.path, e)
                        }
                    }
                }
                #[cfg(not(feature = "video"))]
                if config.background.video.is_some() {
                    log::warn!(
                        "background.video is configured but this build was compiled without the `video` feature; ignoring it (rebuild with --features video)"
                    );
                }
                Box::new(renderer)
            } else {
                let mut renderer = TemplateRenderer::new(
                    &template,
                    source_display.width(),
                    source_display.height(),
                )?;
                renderer.set_layout_vars(layout_vars);
                Box::new(renderer)
            };

            (boxed, None, config.display.tick_rate, resolved_layout)
        };

    // Expand to per-output sources for independent multi-display.
    let mut initial_frame_sources: Vec<Box<dyn FrameSource>> = vec![initial_frame_source];
    let mut independent_layout_specs: Vec<(
        String,
        String,
        std::collections::HashMap<String, String>,
    )> = Vec::new();
    let tick_rotations: Vec<u16>;
    let (initial_xvfb_handle, active_tick_rate, resolved_active_layout) =
        if let Some(specs) = independent_outputs.as_ref() {
            independent_layout_specs = specs
                .iter()
                .map(|spec| {
                    let vars = config
                        .layout_vars
                        .get(&spec.default_layout)
                        .cloned()
                        .unwrap_or_default();
                    (spec.mode.clone(), spec.default_layout.clone(), vars)
                })
                .collect();
            tick_rotations = specs.iter().map(|s| s.rotation).collect();

            let theme_palette: ThemePalette = config
                .theme
                .resolve_palette()
                .map_err(|e| anyhow::anyhow!("invalid theme configuration: {e}"))?;

            let mut sources: Vec<Box<dyn FrameSource>> = Vec::with_capacity(specs.len());
            let mut xvfb_handle = None;
            let mut union_needed: HashSet<String> = HashSet::new();
            let known: HashSet<String> = sensor_hub
                .available_sensors()
                .into_iter()
                .map(|d| d.key)
                .collect();
            let declared: HashSet<String> = sensor_hub.declared_keys();

            for (idx, spec) in specs.iter().enumerate() {
                let dims = if let Some(outputs) = initial_outputs.as_ref() {
                    let info = &outputs
                        .get(idx)
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "independent connect returned fewer outputs than configured targets"
                            )
                        })?
                        .info;
                    let (w, h) = info.oriented_dimensions(spec.rotation)?;
                    RuntimeDisplayDimensions::new(w, h)
                } else {
                    RuntimeDisplayDimensions::new(INTERNAL_CANVAS_W, INTERNAL_CANVAS_H)
                };

                if spec.mode == "xvfb" {
                    if idx != 0 {
                        anyhow::bail!("only displays[0] may use mode=\"xvfb\"");
                    }
                    if config.xvfb.command.is_empty() {
                        anyhow::bail!("xvfb mode requires [xvfb] command in config");
                    }
                    let (handle, source) = dims.start_xvfb_shell(&config.xvfb.command)?;
                    xvfb_handle = Some(handle);
                    sources.push(Box::new(source));
                    continue;
                }

                let layout_path = layout_dir.join(&spec.default_layout);
                let on_disk = if layout_path.exists() {
                    Some(std::fs::read_to_string(&layout_path)?)
                } else {
                    None
                };
                let (resolved_layout, template) =
                    builtin_layouts::resolve_layout_identity(&spec.default_layout, on_disk);
                let frontmatter = LayoutFrontmatter::parse(&template);
                if let Some(ref hist) = initial_sensor_history
                    && let Ok(mut h) = hist.lock()
                {
                    for (metric, cfg) in &frontmatter.history_configs {
                        h.configure_metric(metric, cfg.duration);
                    }
                }
                let layout_vars = config
                    .layout_vars
                    .get(&resolved_layout)
                    .cloned()
                    .unwrap_or_default();
                let needed =
                    layout_needed_keys(&frontmatter, &layout_vars, &template, &known, &declared);
                union_needed.extend(needed);

                let source = dims.build_layout_source_with_bindings(
                    // Build from the resolved layout path so document files are parsed
                    // through the same path as reloads and reconnects.
                    &layout_dir.join(&resolved_layout),
                    layout_vars,
                    initial_background.clone(),
                    initial_sensor_history.clone(),
                    theme_palette.clone(),
                    &declared,
                );
                let source = match source {
                    Ok(s) => s,
                    Err(error) if resolved_layout.ends_with(".layout.toml") => return Err(error),
                    Err(_) => {
                        // Layout may only exist as builtin content — write through identity path.
                        // Fall back to constructing via temporary content already resolved:
                        let is_svg = resolved_layout.ends_with(".svg");
                        if is_svg {
                            let mut renderer =
                                SvgRenderer::new(&template, dims.width(), dims.height())?;
                            if let Some(ref hist) = initial_sensor_history {
                                renderer.set_history(hist.clone());
                            }
                            renderer.set_theme(theme_palette.clone());
                            renderer.set_layout_vars(
                                config
                                    .layout_vars
                                    .get(&resolved_layout)
                                    .cloned()
                                    .unwrap_or_default(),
                            );
                            let _ = renderer.set_background(initial_background.clone());
                            Box::new(renderer) as Box<dyn FrameSource>
                        } else {
                            let mut renderer =
                                TemplateRenderer::new(&template, dims.width(), dims.height())?;
                            renderer.set_layout_vars(
                                config
                                    .layout_vars
                                    .get(&resolved_layout)
                                    .cloned()
                                    .unwrap_or_default(),
                            );
                            Box::new(renderer)
                        }
                    }
                };
                sources.push(source);
            }

            if !union_needed.is_empty() {
                let _ = needed_tx.send(Some(union_needed));
            }
            // Primary layout identity for D-Bus.
            let primary_layout = specs[0].default_layout.clone();
            let tick_rate = if specs.iter().any(|s| s.mode == "xvfb") {
                xvfb_tick_rate
            } else {
                config.display.tick_rate
            };
            initial_frame_sources = sources;
            (xvfb_handle, tick_rate, primary_layout)
        } else {
            tick_rotations = vec![config.display.rotation];
            (
                initial_xvfb_handle,
                active_tick_rate,
                resolved_active_layout,
            )
        };

    // Shared state for D-Bus ↔ tick loop communication
    let state = Arc::new(Mutex::new(ServiceState {
        active_layout: resolved_active_layout.clone(),
        mode: config.display.mode.clone(),
        connected,
        resolution: (display.width(), display.height()),
        display_count,
        tick_rate: config.display.tick_rate,
        jpeg_quality: config.display.jpeg_quality,
        shutdown_tx,
        tick_rate_tx,
        layout_dir: layout_dir.clone(),
        config_path: config_path.clone(),
        sensor_descriptors: Arc::clone(&sensor_descriptors),
        config: config.clone(),
        mode_change_tx: mode_tx,
        background_dir: background_dir.clone(),
        wrapper_dir: wrapper_dir.clone(),
        current_background: initial_background.clone(),
        config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
        bg_change_lock: Arc::new(tokio::sync::Mutex::new(())),
        // Serializes the full set_mode body (channel-send + ack-await + state-mirror)
        // to prevent concurrent start/stop calls from interleaving.
        mode_change_lock: Arc::new(tokio::sync::Mutex::new(())),
        // No pre-stream tick rate yet (daemon starts in layout mode).
        pre_stream_tick_rate: None,
    }));

    {
        let mut connected_rx = connected_tx.subscribe();
        let state_for_connected = Arc::clone(&state);
        tokio::spawn(async move {
            while connected_rx.changed().await.is_ok() {
                let value = *connected_rx.borrow();
                state_for_connected.lock().await.connected = value;
            }
        });
    }

    {
        let mut display_rx = display_tx.subscribe();
        let state_for_display = Arc::clone(&state);
        tokio::spawn(async move {
            while display_rx.changed().await.is_ok() {
                let display = *display_rx.borrow_and_update();
                state_for_display.lock().await.resolution = (display.width(), display.height());
            }
        });
    }

    {
        let mut display_count_rx = display_count_tx.subscribe();
        let state_for_display_count = Arc::clone(&state);
        tokio::spawn(async move {
            while display_count_rx.changed().await.is_ok() {
                let count = *display_count_rx.borrow_and_update();
                state_for_display_count.lock().await.display_count = count;
            }
        });
    }

    // Start D-Bus service (connection must stay alive)
    let _connection = dbus::serve(state.clone()).await?;
    info!("D-Bus service started");

    // Theme palette for the mode-change handler to use on layout reload.
    // Computed here (outside the if/else block) so it can be moved into the spawn closure.
    let reload_theme: ThemePalette = config
        .theme
        .resolve_palette()
        .map_err(|e| anyhow::anyhow!("invalid theme configuration: {e}"))?;

    // Mode change listener: handles layout switches, xvfb mode, background changes,
    // and display-dimension changes after a reconnect.
    let layout_dir_clone = layout_dir.clone();
    let xvfb_tick_rate_cfg = xvfb_tick_rate;
    let reload_history = initial_sensor_history.clone();
    let mut display_rx = display_tx.subscribe();
    let mut generation_rx_listener = generation_rx.clone();
    let source_revision_tx_listener = source_revision_tx.clone();
    let initial_mode = independent_outputs
        .as_ref()
        .and_then(|s| s.first().map(|o| o.mode.clone()))
        .unwrap_or_else(|| config.display.mode.clone());
    let initial_active_layout = resolved_active_layout;
    let initial_layout_vars = config
        .layout_vars
        .get(&initial_active_layout)
        .cloned()
        .unwrap_or_default();
    let declared_keys_listener = Arc::clone(&declared_keys);
    let initial_xvfb_command = (config.display.mode == "xvfb").then(|| config.xvfb.command.clone());
    let needed_tx_listener = needed_tx.clone();
    let sensor_descriptors_listener = Arc::clone(&sensor_descriptors);
    let independent_layout_specs = independent_layout_specs;
    tokio::spawn(async move {
        // xvfb_handle owns the Xvfb process — dropping it kills the process.
        let mut xvfb_handle: Option<thermalwriter::service::xvfb::XvfbHandle> = initial_xvfb_handle;
        // (mode, layout, vars) per independent output; empty in single/mirror mode.
        let mut independent_layout_specs = independent_layout_specs;
        // Tracks the active background so layout switches preserve it.
        let mut current_background: Option<Arc<BackgroundImage>> = initial_background;
        let mut current_display = source_display;
        let mut active_mode = initial_mode;
        let mut active_layout = initial_active_layout;
        let mut active_layout_vars = initial_layout_vars;
        let mut source_revision: u64 = 0;
        let mut active_xvfb_shell = initial_xvfb_command;
        let mut active_xvfb_argv: Option<Vec<String>> = None;
        let _ = generation_rx_listener.borrow_and_update();

        loop {
            tokio::select! {
                // Generation-tagged rebuild requests from the tick loop (reconnect path).
                req = source_build_rx.recv() => {
                    let Some(req) = req else { break; };
                    let result = (|| {
                        let mut built: Vec<Box<dyn FrameSource>> = Vec::with_capacity(req.canvases.len());
                        for (idx, (width, height)) in req.canvases.iter().copied().enumerate() {
                            let dims = RuntimeDisplayDimensions::new(width, height);
                            // Independent secondary outputs use stored per-output layout specs when present.
                            let (mode, layout_name, layout_vars) = if idx == 0 {
                                (active_mode.as_str(), active_layout.as_str(), active_layout_vars.clone())
                            } else if let Some(spec) = independent_layout_specs.get(idx) {
                                (spec.0.as_str(), spec.1.as_str(), spec.2.clone())
                            } else {
                                (active_mode.as_str(), active_layout.as_str(), active_layout_vars.clone())
                            };

                            if mode == "xvfb" {
                                if idx != 0 {
                                    return Err("only the primary output may use xvfb mode".into());
                                }
                                if let Some(argv) = active_xvfb_argv.clone() {
                                    match dims.start_xvfb_argv(&argv) {
                                        Ok((new_handle, source)) => {
                                            if let Some(h) = xvfb_handle.take() {
                                                drop(h);
                                            }
                                            xvfb_handle = Some(new_handle);
                                            current_display = dims;
                                            built.push(Box::new(source));
                                        }
                                        Err(e) => return Err(format!("xvfb argv rebuild failed: {e:#}")),
                                    }
                                } else if let Some(command) = active_xvfb_shell.clone() {
                                    match dims.start_xvfb_shell(&command) {
                                        Ok((new_handle, source)) => {
                                            if let Some(h) = xvfb_handle.take() {
                                                drop(h);
                                            }
                                            xvfb_handle = Some(new_handle);
                                            current_display = dims;
                                            built.push(Box::new(source));
                                        }
                                        Err(e) => return Err(format!("xvfb shell rebuild failed: {e:#}")),
                                    }
                                } else {
                                    return Err("xvfb mode has no tracked stream command".into());
                                }
                            } else {
                                let layout_path = layout_dir_clone.join(layout_name);
                                match dims.build_layout_source_with_bindings(
                                    &layout_path,
                                    layout_vars,
                                    current_background.clone(),
                                    reload_history.clone(),
                                    reload_theme.clone(),
                                    declared_keys_listener.as_ref(),
                                ) {
                                    Ok(source) => {
                                        if idx == 0 {
                                            if let Some(h) = xvfb_handle.take() {
                                                drop(h);
                                            }
                                            current_display = dims;
                                        }
                                        built.push(source);
                                    }
                                    Err(e) => {
                                        return Err(format!(
                                            "layout rebuild for '{layout_name}' failed: {e:#}"
                                        ));
                                    }
                                }
                            }
                        }
                        Ok(built)
                    })();
                    match &result {
                        Ok(sources) => {
                            let (w, h) = req.canvases.first().copied().unwrap_or((0, 0));
                            info!(
                                "Built {} source(s) for generation {} (primary {}x{})",
                                sources.len(),
                                req.generation,
                                w,
                                h
                            );
                        }
                        Err(e) => {
                            log::warn!(
                                "Source build for generation {} failed: {e}",
                                req.generation
                            );
                        }
                    }
                    if source_result_tx
                        .send(SourceBuildResult {
                            generation: req.generation,
                            source_revision,
                            sources: result,
                            commit: None,
                        })
                        .await
                        .is_err()
                    {
                        log::warn!("Tick loop dropped source result channel");
                        break;
                    }
                }
                // Track active generation for layout/mode swaps.
                changed = generation_rx_listener.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let _ = generation_rx_listener.borrow_and_update();
                }
                // Keep display_rx drained so it doesn't lag; dimension rebuilds are
                // driven by SourceBuildRequest from the tick loop, not this channel.
                changed = display_rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let _ = display_rx.borrow_and_update();
                }
                change = mode_rx.recv() => {
                    let Some(change) = change else {
                        break;
                    };
                    match change {
                        ModeChange::Layout { name, vars, ack } => {
                            let layout_path = layout_dir_clone.join(&name);
                            // Prefer active negotiated dims; fall back to last known.
                            let mode_display = if current_display.width() > 0 {
                                current_display
                            } else {
                                *display_rx.borrow()
                            };
                            // Disconnected placeholder: use internal 480 when display is 0x0.
                            let mode_display = if mode_display.width() == 0 || mode_display.height() == 0 {
                                RuntimeDisplayDimensions::new(480, 480)
                            } else {
                                mode_display
                            };
                            let generation = *generation_rx_listener.borrow();
                            match mode_display.build_layout_source_with_bindings(
                                &layout_path,
                                vars.clone(),
                                current_background.clone(),
                                reload_history.clone(),
                                reload_theme.clone(),
                                declared_keys_listener.as_ref(),
                            ) {
                                Ok(new_source) => {
                                    let next_source_revision = source_revision.saturating_add(1);
                                    if let Err(error) = apply_source_revision(
                                        &source_revision_tx_listener,
                                        next_source_revision,
                                        generation == 0,
                                    )
                                    .await
                                    {
                                        let _ = ack.send(Err(error));
                                        continue;
                                    }
                                    source_revision = next_source_revision;
                                    if generation == 0 {
                                        // Disconnected: record layout for next reconnect rebuild.
                                        drop(new_source);
                                        if let Some(h) = xvfb_handle.take() {
                                            drop(h);
                                        }
                                        active_mode = if name.ends_with(".html") {
                                            "html".to_string()
                                        } else {
                                            "svg".to_string()
                                        };
                                        active_layout = name.clone();
                                        send_layout_needed_keys(
                                            &layout_path,
                                            &vars,
                                            &sensor_descriptors_listener,
                                            &declared_keys_listener,
                                            &needed_tx_listener,
                                            &recipe_tx,
                                        );
                                        active_layout_vars = vars;
                                        active_xvfb_shell = None;
                                        active_xvfb_argv = None;
                                        if let Ok(template) = std::fs::read_to_string(&layout_path) {
                                            let _ = template_tx.send(template);
                                        }
                                        info!("Recorded layout '{}' while disconnected", name);
                                        let _ = ack.send(Ok(()));
                                        continue;
                                    }
                                    if let Err(error) = commit_frame_source(
                                        &source_result_tx,
                                        generation,
                                        source_revision,
                                        vec![new_source],
                                    )
                                    .await
                                    {
                                        log::warn!("Layout source commit failed: {error:#}");
                                        let _ = ack.send(Err(error));
                                        continue;
                                    }
                                    if let Some(h) = xvfb_handle.take() {
                                        drop(h);
                                    }
                                    current_display = mode_display;
                                    active_mode = if name.ends_with(".html") {
                                        "html".to_string()
                                    } else {
                                        "svg".to_string()
                                    };
                                    active_layout = name.clone();
                                    if let Some(spec) = independent_layout_specs.get_mut(0) {
                                        spec.0 = active_mode.clone();
                                        spec.1 = name.clone();
                                        spec.2 = vars.clone();
                                    }
                                    send_layout_needed_keys(
                                        &layout_path,
                                        &vars,
                                        &sensor_descriptors_listener,
                                        &declared_keys_listener,
                                        &needed_tx_listener,
                                        &recipe_tx,
                                    );
                                    active_layout_vars = vars;
                                    active_xvfb_shell = None;
                                    active_xvfb_argv = None;
                                    if let Ok(template) = std::fs::read_to_string(&layout_path) {
                                        let _ = template_tx.send(template);
                                    }
                                    info!("Switched to layout: {} (gen {generation})", name);
                                    let _ = ack.send(Ok(()));
                                }
                                Err(e) => {
                                    log::warn!("Layout transition failed for '{}': {}", name, e);
                                    let _ = ack.send(Err(e));
                                }
                            }
                        }
                        ModeChange::Background { image, ack } => {
                            // Forward to tick for transactional set_background; only
                            // update local cache on success so reconnect rebuilds match.
                            let (inner_ack_tx, inner_ack_rx) = tokio::sync::oneshot::channel();
                            if background_apply_tx
                                .send(BackgroundApply {
                                    image: image.clone(),
                                    ack: inner_ack_tx,
                                })
                                .await
                                .is_err()
                            {
                                let _ = ack.send(Err(anyhow::anyhow!(
                                    "tick loop dropped background apply channel"
                                )));
                                continue;
                            }
                            match inner_ack_rx.await {
                                Ok(Ok(())) => {
                                    current_background = image.clone();
                                    let _ = background_tx.send(image);
                                    info!(
                                        "Background updated ({})",
                                        if current_background.is_some() {
                                            "set"
                                        } else {
                                            "cleared"
                                        }
                                    );
                                    let _ = ack.send(Ok(()));
                                }
                                Ok(Err(e)) => {
                                    let _ = ack.send(Err(anyhow::anyhow!("{e}")));
                                }
                                Err(_) => {
                                    let _ = ack.send(Err(anyhow::anyhow!(
                                        "background apply ack dropped"
                                    )));
                                }
                            }
                        }
                        ModeChange::Xvfb { command, ack } => {
                            let mode_display = if current_display.width() > 0 {
                                current_display
                            } else {
                                RuntimeDisplayDimensions::new(480, 480)
                            };
                            let generation = *generation_rx_listener.borrow();
                            if generation == 0 {
                                let _ = ack.send(Err(anyhow::anyhow!(
                                    "cannot start xvfb while disconnected"
                                )));
                                continue;
                            }
                            match mode_display.start_xvfb_shell(&command) {
                                Ok((new_handle, source)) => {
                                    let next_source_revision = source_revision.saturating_add(1);
                                    if let Err(error) = apply_source_revision(
                                        &source_revision_tx_listener,
                                        next_source_revision,
                                        false,
                                    )
                                    .await
                                    {
                                        let _ = ack.send(Err(error));
                                        continue;
                                    }
                                    source_revision = next_source_revision;
                                    if let Err(error) = commit_frame_source(
                                        &source_result_tx,
                                        generation,
                                        source_revision,
                                        vec![Box::new(source)],
                                    )
                                    .await
                                    {
                                        log::warn!("Xvfb source commit failed: {error:#}");
                                        let _ = ack.send(Err(error));
                                        continue;
                                    }
                                    if let Some(h) = xvfb_handle.take() {
                                        drop(h);
                                    }
                                    xvfb_handle = Some(new_handle);
                                    current_display = mode_display;
                                    active_mode = "xvfb".to_string();
                                    // Xvfb mode: sensors unused for frame content.
                                    let _ = needed_tx_listener.send(Some(HashSet::new()));
                                    active_xvfb_shell = Some(command.clone());
                                    active_xvfb_argv = None;
                                    info!(
                                        "Switched to xvfb mode: {} ({}fps, gen {generation})",
                                        command, xvfb_tick_rate_cfg
                                    );
                                    let _ = ack.send(Ok(()));
                                }
                                Err(e) => {
                                    let msg =
                                        format!("Failed to start xvfb for command '{}': {}", command, e);
                                    log::warn!("{}", msg);
                                    let _ = ack.send(Err(anyhow::anyhow!("{}", msg)));
                                }
                            }
                        }
                        ModeChange::XvfbArgv { argv, ack } => {
                            let mode_display = if current_display.width() > 0 {
                                current_display
                            } else {
                                RuntimeDisplayDimensions::new(480, 480)
                            };
                            let generation = *generation_rx_listener.borrow();
                            if generation == 0 {
                                let _ = ack.send(Err(anyhow::anyhow!(
                                    "cannot start xvfb while disconnected"
                                )));
                                continue;
                            }
                            match mode_display.start_xvfb_argv(&argv) {
                                Ok((new_handle, source)) => {
                                    let next_source_revision = source_revision.saturating_add(1);
                                    if let Err(error) = apply_source_revision(
                                        &source_revision_tx_listener,
                                        next_source_revision,
                                        false,
                                    )
                                    .await
                                    {
                                        let _ = ack.send(Err(error));
                                        continue;
                                    }
                                    source_revision = next_source_revision;
                                    if let Err(error) = commit_frame_source(
                                        &source_result_tx,
                                        generation,
                                        source_revision,
                                        vec![Box::new(source)],
                                    )
                                    .await
                                    {
                                        log::warn!("Xvfb argv source commit failed: {error:#}");
                                        let _ = ack.send(Err(error));
                                        continue;
                                    }
                                    if let Some(h) = xvfb_handle.take() {
                                        drop(h);
                                    }
                                    xvfb_handle = Some(new_handle);
                                    current_display = mode_display;
                                    active_mode = "xvfb".to_string();
                                    active_xvfb_shell = None;
                                    active_xvfb_argv = Some(argv.clone());
                                    // Xvfb mode: sensors unused for frame content.
                                    let _ = needed_tx_listener.send(Some(HashSet::new()));
                                    info!(
                                        "Switched to xvfb mode (argv): {:?} ({}fps, gen {generation})",
                                        argv, xvfb_tick_rate_cfg
                                    );
                                    let _ = ack.send(Ok(()));
                                }
                                Err(e) => {
                                    let msg = format!("Failed to start xvfb for argv {:?}: {}", argv, e);
                                    log::warn!("{}", msg);
                                    let _ = ack.send(Err(anyhow::anyhow!("{}", msg)));
                                }
                            }
                        }
                    }
                }
            }
        }
    });

    // Run tick loop — blocks until shutdown signal or process signal
    let jpeg_quality = state.lock().await.jpeg_quality;
    let sensor_poll_interval = Duration::from_millis(config.sensors.poll_interval_ms);

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("install SIGTERM handler");

    tokio::select! {
        res = tick::run_tick_loop(
            initial_outputs,
            device_info,
            connector,
            initial_frame_sources,
            independent,
            source_build_tx,
            &mut source_result_rx,
            &mut sensor_hub,
            active_tick_rate,
            jpeg_quality,
            tick_rotations,
            template_rx,
            background_rx,
            &mut background_apply_rx,
            shutdown_rx,
            initial_sensor_history,
            sensor_poll_interval,
            Some(Arc::clone(&sensor_descriptors)),
            connected_tx,
            display_tx,
            display_count_tx,
            generation_tx,
            &mut source_revision_rx,
            tick_rate_rx,
            needed_rx,
            recipe_rx,
        ) => { res?; }
        _ = tokio::signal::ctrl_c() => {
            info!("SIGINT received, shutting down");
        }
        _ = sigterm.recv() => {
            info!("SIGTERM received, shutting down");
        }
    }

    // Signal the tick loop to stop (no-op if it already exited normally)
    let _ = state.lock().await.shutdown_tx.send(true);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    info!("thermalwriter shutdown complete");
    Ok(())
}
