use anyhow::Result;
use clap::Parser;
use log::{info, warn};
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
use thermalwriter::sensor::amdgpu::AmdGpuProvider;
use thermalwriter::sensor::history::SensorHistory;
use thermalwriter::sensor::hwmon::HwmonProvider;
use thermalwriter::sensor::mangohud::MangoHudProvider;
use thermalwriter::sensor::nvidia::NvidiaProvider;
use thermalwriter::sensor::rapl::RaplProvider;
use thermalwriter::sensor::sysinfo_provider::SysinfoProvider;
use thermalwriter::service::dbus::{self, ModeChange, ServiceState};
use thermalwriter::service::mode_handler::RuntimeDisplayDimensions;
use thermalwriter::service::tick::{
    self, BackgroundApply, SourceBuildRequest, SourceBuildResult, SourceRevisionApply,
};
use thermalwriter::theme::ThemePalette;
use thermalwriter::transport::DeviceInfo;
use thermalwriter::transport::Transport;
use thermalwriter::transport::discovery::{DeviceSelector, TransportConnector};

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
    source: Box<dyn FrameSource>,
) -> Result<()> {
    let (commit_tx, commit_rx) = tokio::sync::oneshot::channel();
    sender
        .send(SourceBuildResult {
            generation,
            source_revision,
            source: Ok(source),
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
    let connector =
        TransportConnector::from_config_device(&config.display.device).unwrap_or_else(|e| {
            warn!(
                "Invalid display.device={:?}: {e:#}; falling back to auto",
                config.display.device
            );
            TransportConnector::new(DeviceSelector::Auto)
        });
    let (transport, device_info, connected, display): (
        Option<Box<dyn Transport>>,
        Option<DeviceInfo>,
        bool,
        RuntimeDisplayDimensions,
    ) = match connector.connect() {
        Ok((t, info)) => {
            info!(
                "Device: {}x{}, PM={}, SUB={}, FBL={}, protocol={}, encoding={}",
                info.width(),
                info.height(),
                info.pm,
                info.sub,
                info.fbl,
                info.protocol,
                info.encoding()
            );
            (
                Some(t),
                Some(info.clone()),
                true,
                RuntimeDisplayDimensions::new(info.width(), info.height()),
            )
        }
        Err(e) => {
            warn!("Display unavailable at startup: {e:#}; daemon will keep running and retry");
            // Disconnected publishes (0,0); 480×480 is internal-only for layout authoring.
            (None, None, false, RuntimeDisplayDimensions::new(0, 0))
        }
    };

    const INTERNAL_CANVAS_W: u32 = 480;
    const INTERNAL_CANVAS_H: u32 = 480;
    // Internal authoring canvas when disconnected; never published on D-Bus as connected dims.
    let source_display = if let Some(info) = device_info.as_ref() {
        let (width, height) = info.oriented_dimensions(config.display.rotation)?;
        RuntimeDisplayDimensions::new(width, height)
    } else {
        RuntimeDisplayDimensions::new(INTERNAL_CANVAS_W, INTERNAL_CANVAS_H)
    };

    // Setup sensor hub with all providers
    let mut sensor_hub = SensorHub::new();
    sensor_hub.add_provider(Box::new(HwmonProvider::new()));
    sensor_hub.add_provider(Box::new(SysinfoProvider::new()));
    // Nvidia before AmdGpu so a hybrid (AMD iGPU + NVIDIA dGPU) machine reports
    // the discrete GPU that users care about on the cooler LCD. On pure-AMD
    // systems Nvidia returns empty and AmdGpu still owns the keys.
    sensor_hub.add_provider(Box::new(NvidiaProvider::new()));
    sensor_hub.add_provider(Box::new(AmdGpuProvider::new()));
    sensor_hub.add_provider(Box::new(MangoHudProvider::from_configured_dir(
        &config.sensors.mangohud_log_dir,
    )));
    sensor_hub.add_provider(Box::new(RaplProvider::new()));

    // Prime providers so they discover devices, then snapshot descriptors for
    // the D-Bus list_sensors method. Must happen before the D-Bus service
    // starts so the first client call sees real data.
    let _ = sensor_hub.poll();
    let sensor_descriptors: Vec<(String, String, String)> = sensor_hub
        .available_sensors()
        .into_iter()
        .map(|d| (d.key, d.name, d.unit))
        .collect();

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
    let (template_tx, template_rx) = watch::channel(String::new());
    // Background watch channel: mode-change listener → tick loop (immediate apply)
    let (background_tx, background_rx) =
        watch::channel::<Option<Arc<BackgroundImage>>>(initial_background.clone());
    // Tick rate watch channel: D-Bus set_tick_rate → tick loop (no restart needed)
    let (tick_rate_tx, tick_rate_rx) = watch::channel::<u32>(config.display.tick_rate);
    // Mode change channel (D-Bus → listener task)
    let (mode_tx, mut mode_rx) = mpsc::channel::<ModeChange>(4);

    // Determine initial frame source, tick rate, and sensor history based on config mode
    let xvfb_tick_rate = config.xvfb.tick_rate.clamp(1, 60);
    // Always allocate SensorHistory so a later D-Bus set_layout into a history-using layout
    // populates correctly even when starting in xvfb mode.
    let initial_sensor_history: Option<Arc<std::sync::Mutex<SensorHistory>>> =
        Some(Arc::new(std::sync::Mutex::new(SensorHistory::new())));
    let (initial_frame_source, initial_xvfb_handle, active_tick_rate, resolved_active_layout) =
        if config.display.mode == "xvfb" {
            if config.xvfb.command.is_empty() {
                anyhow::bail!("xvfb mode requires [xvfb] command in config");
            }
            let (handle, source) = source_display.start_xvfb_shell(&config.xvfb.command)?;
            let boxed: Box<dyn FrameSource> = Box::new(source);
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

            let is_svg = resolved_layout.ends_with(".svg");
            let boxed: Box<dyn FrameSource> = if is_svg {
                let mut renderer =
                    SvgRenderer::new(&template, source_display.width(), source_display.height())?;
                if let Some(ref hist) = initial_sensor_history {
                    renderer.set_history(hist.clone());
                }
                renderer.set_theme(theme_palette);
                renderer.set_layout_vars(layout_vars);
                let _ = renderer.set_background(initial_background.clone());
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

    // Shared state for D-Bus ↔ tick loop communication
    let state = Arc::new(Mutex::new(ServiceState {
        active_layout: resolved_active_layout.clone(),
        mode: config.display.mode.clone(),
        connected,
        resolution: (display.width(), display.height()),
        tick_rate: config.display.tick_rate,
        jpeg_quality: config.display.jpeg_quality,
        shutdown_tx,
        tick_rate_tx,
        layout_dir: layout_dir.clone(),
        config_path: config_path.clone(),
        sensor_descriptors,
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
    let initial_mode = config.display.mode.clone();
    let initial_active_layout = resolved_active_layout;
    let initial_layout_vars = config
        .layout_vars
        .get(&initial_active_layout)
        .cloned()
        .unwrap_or_default();
    let initial_xvfb_command = (config.display.mode == "xvfb").then(|| config.xvfb.command.clone());
    tokio::spawn(async move {
        // xvfb_handle owns the Xvfb process — dropping it kills the process.
        let mut xvfb_handle: Option<thermalwriter::service::xvfb::XvfbHandle> = initial_xvfb_handle;
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
                    let dims = RuntimeDisplayDimensions::new(req.width, req.height);
                    let result = if active_mode == "xvfb" {
                        if let Some(argv) = active_xvfb_argv.clone() {
                            match dims.start_xvfb_argv(&argv) {
                                Ok((new_handle, source)) => {
                                    if let Some(h) = xvfb_handle.take() {
                                        drop(h);
                                    }
                                    xvfb_handle = Some(new_handle);
                                    current_display = dims;
                                    Ok(Box::new(source) as Box<dyn FrameSource>)
                                }
                                Err(e) => Err(format!("xvfb argv rebuild failed: {e:#}")),
                            }
                        } else if let Some(command) = active_xvfb_shell.clone() {
                            match dims.start_xvfb_shell(&command) {
                                Ok((new_handle, source)) => {
                                    if let Some(h) = xvfb_handle.take() {
                                        drop(h);
                                    }
                                    xvfb_handle = Some(new_handle);
                                    current_display = dims;
                                    Ok(Box::new(source) as Box<dyn FrameSource>)
                                }
                                Err(e) => Err(format!("xvfb shell rebuild failed: {e:#}")),
                            }
                        } else {
                            Err("xvfb mode has no tracked stream command".into())
                        }
                    } else {
                        let layout_path = layout_dir_clone.join(&active_layout);
                        match dims.build_layout_source(
                            &layout_path,
                            active_layout_vars.clone(),
                            current_background.clone(),
                            reload_history.clone(),
                            reload_theme.clone(),
                        ) {
                            Ok(source) => {
                                if let Some(h) = xvfb_handle.take() {
                                    drop(h);
                                }
                                current_display = dims;
                                Ok(source)
                            }
                            Err(e) => Err(format!(
                                "layout rebuild for '{}' failed: {e:#}",
                                active_layout
                            )),
                        }
                    };
                    if result.is_ok() {
                        info!(
                            "Built source for generation {} at {}x{}",
                            req.generation, req.width, req.height
                        );
                    } else if let Err(ref e) = result {
                        log::warn!(
                            "Source build for generation {} failed: {e}",
                            req.generation
                        );
                    }
                    if source_result_tx
                        .send(SourceBuildResult {
                            generation: req.generation,
                            source_revision,
                            source: result,
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
                            match mode_display.build_layout_source(
                                &layout_path,
                                vars.clone(),
                                current_background.clone(),
                                reload_history.clone(),
                                reload_theme.clone(),
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
                                        new_source,
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
                                        Box::new(source),
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
                                        Box::new(source),
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
    let rotation = config.display.rotation;
    let sensor_poll_interval = Duration::from_millis(config.sensors.poll_interval_ms);

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("install SIGTERM handler");

    tokio::select! {
        res = tick::run_tick_loop(
            transport,
            device_info,
            connector,
            initial_frame_source,
            source_build_tx,
            &mut source_result_rx,
            &mut sensor_hub,
            active_tick_rate,
            jpeg_quality,
            rotation,
            template_rx,
            background_rx,
            &mut background_apply_rx,
            shutdown_rx,
            initial_sensor_history,
            sensor_poll_interval,
            connected_tx,
            display_tx,
            generation_tx,
            &mut source_revision_rx,
            tick_rate_rx,
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
