use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use anyhow::Result;
use clap::Parser;
use log::info;
use tokio::sync::{Mutex, watch, mpsc};

use thermalwriter::cli::{Cli, Command};
use thermalwriter::config::{Config, builtin_layouts};
use thermalwriter::render::{FrameSource, background as bg_decode};
use thermalwriter::render::TemplateRenderer;
use thermalwriter::render::frontmatter::LayoutFrontmatter;
use thermalwriter::render::svg::SvgRenderer;
use thermalwriter::render::xvfb::XvfbSource;
use thermalwriter::sensor::SensorHub;
use thermalwriter::sensor::history::SensorHistory;
use thermalwriter::sensor::hwmon::HwmonProvider;
use thermalwriter::sensor::sysinfo_provider::SysinfoProvider;
use thermalwriter::sensor::amdgpu::AmdGpuProvider;
use thermalwriter::sensor::nvidia::NvidiaProvider;
use thermalwriter::sensor::mangohud::MangoHudProvider;
use thermalwriter::sensor::rapl::RaplProvider;
use thermalwriter::service::dbus::{self, ModeChange, ServiceState};
use thermalwriter::service::tick;
use thermalwriter::service::xvfb as xvfb_manager;
use thermalwriter::theme::ThemePalette;
use thermalwriter::transport::Transport;
use thermalwriter::transport::bulk_usb::BulkUsb;

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

    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()))
        .join("thermalwriter");
    let layout_dir = config_dir.join("layouts");
    std::fs::create_dir_all(&layout_dir)?;

    // Load config (defaults if file missing, error if invalid TOML)
    let config_path = config_dir.join("config.toml");
    let config = Config::load(&config_path)?;
    info!("Config: tick_rate={}, layout={}, jpeg_quality={}, mode={}",
          config.display.tick_rate, config.display.default_layout,
          config.display.jpeg_quality, config.display.mode);

    // Seed built-in layouts on first run (only if files don't exist)
    builtin_layouts::seed_layout_dir(&layout_dir)?;

    // Seed built-in background images on first run
    let background_dir = config_dir.join("backgrounds");
    std::fs::create_dir_all(&background_dir)?;
    builtin_layouts::seed_background_dir(&background_dir)?;

    // Decode the configured background image at startup (if set)
    let initial_background: Option<tiny_skia::Pixmap> =
        if let Some(ref image_name) = config.background.image {
            let bg_path = background_dir.join(image_name);
            match bg_decode::decode_from_file(&bg_path) {
                Ok(pixmap) => Some(pixmap),
                Err(e) => {
                    log::warn!("Failed to decode background '{}': {}", image_name, e);
                    None
                }
            }
        } else {
            None
        };

    // Setup USB transport
    let mut transport = BulkUsb::new()?;
    let device_info = transport.handshake()?;
    info!("Device: {}x{}, PM={}, JPEG={}", device_info.width, device_info.height,
          device_info.pm, device_info.use_jpeg);

    // Setup sensor hub with all providers
    let mut sensor_hub = SensorHub::new();
    sensor_hub.add_provider(Box::new(HwmonProvider::new()));
    sensor_hub.add_provider(Box::new(SysinfoProvider::new()));
    sensor_hub.add_provider(Box::new(AmdGpuProvider::new()));
    sensor_hub.add_provider(Box::new(NvidiaProvider::new()));
    sensor_hub.add_provider(Box::new(MangoHudProvider::new()));
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

    // Channel for hot-swapping the frame source from the mode change listener
    let (source_tx, mut source_rx) = mpsc::channel::<Box<dyn FrameSource>>(1);
    // Shared shutdown + template channels
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (connected_tx, _) = watch::channel(true);
    let (template_tx, template_rx) = watch::channel(String::new());
    // Background watch channel: mode-change listener → tick loop (immediate apply)
    let (background_tx, background_rx) = watch::channel::<Option<tiny_skia::Pixmap>>(initial_background.clone());
    // Tick rate watch channel: D-Bus set_tick_rate → tick loop (no restart needed)
    let (tick_rate_tx, tick_rate_rx) = watch::channel::<u32>(config.display.tick_rate);
    // Mode change channel (D-Bus → listener task)
    let (mode_tx, mut mode_rx) = mpsc::channel::<ModeChange>(4);

    // Determine initial frame source, tick rate, and sensor history based on config mode
    let xvfb_tick_rate = config.xvfb.tick_rate.min(60).max(1);
    // Always allocate SensorHistory so a later D-Bus set_layout into a history-using layout
    // populates correctly even when starting in xvfb mode.
    let initial_sensor_history: Option<Arc<std::sync::Mutex<SensorHistory>>> =
        Some(Arc::new(std::sync::Mutex::new(SensorHistory::new())));
    let (initial_frame_source, initial_xvfb_handle, active_tick_rate) =
        if config.display.mode == "xvfb" {
            if config.xvfb.command.is_empty() {
                anyhow::bail!("xvfb mode requires [xvfb] command in config");
            }
            let handle = xvfb_manager::start(&config.xvfb.command, device_info.width, device_info.height)?;
            let source = XvfbSource::new(handle.screen_file(), device_info.width, device_info.height)?;
            let boxed: Box<dyn FrameSource> = Box::new(source);
            // History is allocated but not seeded with layout frontmatter — metrics get configured
            // when the user switches to a layout via D-Bus set_layout.
            (boxed, Some(handle), xvfb_tick_rate)
        } else {
            // Load configured layout — user file takes precedence over built-in
            let layout_path = layout_dir.join(&config.display.default_layout);
            let template = if layout_path.exists() {
                std::fs::read_to_string(&layout_path)?
            } else {
                builtin_layouts::SYSTEM_STATS.to_string()
            };

            let frontmatter = LayoutFrontmatter::parse(&template);
            // Configure history metrics from the layout's frontmatter into the
            // already-allocated shared SensorHistory.
            if let Some(ref hist) = initial_sensor_history {
                if let Ok(mut h) = hist.lock() {
                    for (metric, cfg) in &frontmatter.history_configs {
                        h.configure_metric(metric, cfg.duration);
                    }
                }
            }

            let theme_palette = if let Some(manual) = config.theme.manual.clone() {
                manual
            } else {
                ThemePalette::default()
            };

            let is_svg = config.display.default_layout.ends_with(".svg");
            let boxed: Box<dyn FrameSource> = if is_svg {
                let mut renderer = SvgRenderer::new(&template, device_info.width, device_info.height)?;
                if let Some(ref hist) = initial_sensor_history {
                    renderer.set_history(hist.clone());
                }
                renderer.set_theme(theme_palette);
                renderer.set_layout_vars(
                    config.layout_vars
                        .get(&config.display.default_layout)
                        .cloned()
                        .unwrap_or_default(),
                );
                renderer.set_background(initial_background.clone());
                Box::new(renderer)
            } else {
                Box::new(TemplateRenderer::new(&template, device_info.width, device_info.height)?)
            };

            (boxed, None, config.display.tick_rate)
        };

    // Shared state for D-Bus ↔ tick loop communication
    let state = Arc::new(Mutex::new(ServiceState {
        active_layout: config.display.default_layout.clone(),
        mode: config.display.mode.clone(),
        connected: true,
        resolution: (device_info.width, device_info.height),
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
        current_background: initial_background.clone(),
        config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
        bg_change_lock: Arc::new(tokio::sync::Mutex::new(())),
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

    // Start D-Bus service (connection must stay alive)
    let _connection = dbus::serve(state.clone()).await?;
    info!("D-Bus service started");

    // Theme palette for the mode-change handler to use on layout reload.
    // Computed here (outside the if/else block) so it can be moved into the spawn closure.
    let reload_theme = if let Some(manual) = config.theme.manual.clone() {
        manual
    } else {
        ThemePalette::default()
    };

    // Mode change listener: handles layout switches, xvfb mode, and background changes.
    let layout_dir_clone = layout_dir.clone();
    let xvfb_tick_rate_cfg = xvfb_tick_rate;
    let reload_history = initial_sensor_history.clone();
    tokio::spawn(async move {
        // xvfb_handle owns the Xvfb process — dropping it kills the process.
        let mut xvfb_handle: Option<thermalwriter::service::xvfb::XvfbHandle> = initial_xvfb_handle;
        // Tracks the active background so layout switches preserve it.
        let mut current_background: Option<tiny_skia::Pixmap> = initial_background;

        while let Some(change) = mode_rx.recv().await {
            match change {
                ModeChange::Layout { name, vars } => {
                    // Drop any running xvfb before switching back to layout mode
                    if let Some(h) = xvfb_handle.take() { drop(h); }
                    let path = layout_dir_clone.join(&name);
                    match std::fs::read_to_string(&path) {
                        Ok(template) => {
                            // Parse frontmatter and register any new history metrics.
                            // configure_metric is idempotent — existing buffers are preserved.
                            let new_fm = LayoutFrontmatter::parse(&template);
                            if let Some(ref hist) = reload_history {
                                if let Ok(mut h) = hist.lock() {
                                    for (metric, cfg) in &new_fm.history_configs {
                                        h.configure_metric(metric, cfg.duration);
                                    }
                                }
                            }
                            let is_svg = name.ends_with(".svg");
                            let new_source: Box<dyn FrameSource> = if is_svg {
                                match SvgRenderer::new(&template, 480, 480) {
                                    Ok(mut r) => {
                                        r.set_theme(reload_theme.clone());
                                        if let Some(ref hist) = reload_history {
                                            r.set_history(hist.clone());
                                        }
                                        r.set_layout_vars(vars);
                                        // Preserve background across layout switches
                                        r.set_background(current_background.clone());
                                        Box::new(r)
                                    }
                                    Err(e) => {
                                        log::warn!("Failed to create SvgRenderer for {}: {}", name, e);
                                        continue;
                                    }
                                }
                            } else {
                                match TemplateRenderer::new(&template, 480, 480) {
                                    Ok(r) => Box::new(r),
                                    Err(e) => {
                                        log::warn!("Failed to create TemplateRenderer for {}: {}", name, e);
                                        continue;
                                    }
                                }
                            };
                            if source_tx.send(new_source).await.is_err() {
                                log::warn!("Failed to send new frame source to tick loop — receiver dropped");
                            }
                            // Also push raw template for set_template hot-swap path
                            let _ = template_tx.send(template);
                            info!("Switched to layout: {}", name);
                        }
                        Err(e) => log::warn!("Failed to read layout {}: {}", name, e),
                    }
                }
                ModeChange::Background { image } => {
                    current_background = image.clone();
                    // Push to tick loop immediately so the running renderer updates
                    // without waiting for a layout switch.
                    let _ = background_tx.send(image.clone());
                    info!("Background updated ({})", if image.is_some() { "set" } else { "cleared" });
                }
                ModeChange::Xvfb { command } => {
                    // Drop previous xvfb handle before starting a new one
                    if let Some(h) = xvfb_handle.take() { drop(h); }
                    match xvfb_manager::start(&command, 480, 480) {
                        Ok(handle) => {
                            match XvfbSource::new(handle.screen_file(), 480, 480) {
                                Ok(source) => {
                                    if source_tx.send(Box::new(source)).await.is_err() {
                                        log::warn!("Failed to send xvfb frame source to tick loop — receiver dropped");
                                    }
                                    xvfb_handle = Some(handle);
                                    info!("Switched to xvfb mode: {} ({}fps)", command, xvfb_tick_rate_cfg);
                                }
                                Err(e) => log::warn!("Failed to create XvfbSource: {}", e),
                            }
                        }
                        Err(e) => log::warn!("Failed to start xvfb: {}", e),
                    }
                }
            }
        }
    });

    // Run tick loop — blocks until shutdown signal or process signal
    let jpeg_quality = state.lock().await.jpeg_quality;
    let rotation = config.display.rotation;
    let sensor_poll_interval = Duration::from_millis(config.sensors.poll_interval_ms);

    let mut sigterm = tokio::signal::unix::signal(
        tokio::signal::unix::SignalKind::terminate()
    ).expect("install SIGTERM handler");

    tokio::select! {
        res = tick::run_tick_loop(
            &mut transport,
            initial_frame_source,
            &mut source_rx,
            &mut sensor_hub,
            active_tick_rate,
            jpeg_quality,
            rotation,
            template_rx,
            background_rx,
            shutdown_rx,
            initial_sensor_history,
            sensor_poll_interval,
            connected_tx,
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
    transport.close();
    info!("thermalwriter shutdown complete");
    Ok(())
}
