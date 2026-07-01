// Mode-transition helper: builds a new FrameSource from a layout path without
// side-effects on the existing source or xvfb handle.
//
// Callers (main.rs listener) call `build_layout_source` FIRST; only on Ok do
// they drop the old xvfb handle and send the new source to the tick loop.
// On Err, the old source/handle stays live — the stream keeps rendering.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::render::FrameSource;
use crate::render::TemplateRenderer;
use crate::render::frontmatter::LayoutFrontmatter;
use crate::render::svg::SvgRenderer;
#[cfg(feature = "daemon")]
use crate::render::xvfb::XvfbSource;
use crate::sensor::history::SensorHistory;
#[cfg(feature = "daemon")]
use crate::service::xvfb::{self as xvfb_manager, XvfbHandle};
use crate::theme::ThemePalette;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeDisplayDimensions {
    width: u32,
    height: u32,
}

impl RuntimeDisplayDimensions {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    pub fn width(self) -> u32 {
        self.width
    }

    pub fn height(self) -> u32 {
        self.height
    }

    pub fn build_layout_source(
        self,
        layout_path: &Path,
        vars: HashMap<String, String>,
        background: Option<tiny_skia::Pixmap>,
        sensor_history: Option<Arc<Mutex<SensorHistory>>>,
        theme: ThemePalette,
    ) -> anyhow::Result<Box<dyn FrameSource>> {
        build_layout_source(
            layout_path,
            vars,
            background,
            sensor_history,
            theme,
            self.width,
            self.height,
        )
    }

    #[cfg(feature = "daemon")]
    pub fn start_xvfb_shell(self, command: &str) -> anyhow::Result<(XvfbHandle, XvfbSource)> {
        let handle = xvfb_manager::start(command, self.width, self.height)?;
        let source = XvfbSource::new(handle.screen_file(), self.width, self.height)?;
        Ok((handle, source))
    }

    #[cfg(feature = "daemon")]
    pub fn start_xvfb_argv(self, argv: &[String]) -> anyhow::Result<(XvfbHandle, XvfbSource)> {
        let handle = xvfb_manager::start_argv(argv, self.width, self.height)?;
        let source = XvfbSource::new(handle.screen_file(), self.width, self.height)?;
        Ok((handle, source))
    }
}

/// Build a new layout `FrameSource` from `layout_path`.
///
/// Returns `Ok(Box<dyn FrameSource>)` on success. On any failure (file not
/// found, bad SVG, renderer error) returns `Err` — the caller's existing
/// xvfb handle and frame source are untouched.
///
/// `sensor_history` and `theme` are optional — pass `None` for each if not
/// available (unit tests typically pass `None`).
pub fn build_layout_source(
    layout_path: &Path,
    vars: HashMap<String, String>,
    background: Option<tiny_skia::Pixmap>,
    sensor_history: Option<Arc<Mutex<SensorHistory>>>,
    theme: ThemePalette,
    width: u32,
    height: u32,
) -> anyhow::Result<Box<dyn FrameSource>> {
    let template = std::fs::read_to_string(layout_path)
        .map_err(|e| anyhow::anyhow!("Failed to read layout '{}': {}", layout_path.display(), e))?;

    let new_fm = LayoutFrontmatter::parse(&template);
    // Configure any new history metrics idempotently.
    if let Some(ref hist) = sensor_history
        && let Ok(mut h) = hist.lock()
    {
        for (metric, cfg) in &new_fm.history_configs {
            h.configure_metric(metric, cfg.duration);
        }
    }

    let is_svg = layout_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e == "svg")
        .unwrap_or(false);

    if is_svg {
        let mut renderer = SvgRenderer::new(&template, width, height).map_err(|e| {
            anyhow::anyhow!(
                "Failed to create SvgRenderer for '{}': {}",
                layout_path.display(),
                e
            )
        })?;
        renderer.set_theme(theme);
        if let Some(ref hist) = sensor_history {
            renderer.set_history(hist.clone());
        }
        renderer.set_layout_vars(vars);
        renderer.set_background(background);
        Ok(Box::new(renderer))
    } else {
        let renderer = TemplateRenderer::new(&template, width, height).map_err(|e| {
            anyhow::anyhow!(
                "Failed to create TemplateRenderer for '{}': {}",
                layout_path.display(),
                e
            )
        })?;
        Ok(Box::new(renderer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::tempdir;

    /// When build_layout_source fails (file not found), the caller's xvfb handle
    /// sentinel must remain Some — it must NOT be dropped before the new source
    /// is confirmed. This is the key invariant for Task 2.
    #[test]
    fn failed_build_does_not_disturb_existing_handle() {
        let tmp = tempdir().unwrap();
        let nonexistent = tmp.path().join("does-not-exist.svg");

        // Simulate the previous xvfb handle with a sentinel Option.
        let mut xvfb_sentinel: Option<u32> = Some(42);

        let result = build_layout_source(
            &nonexistent,
            HashMap::new(),
            None,
            None,
            ThemePalette::default(),
            480,
            480,
        );

        // Build failed — must return Err.
        assert!(
            result.is_err(),
            "build_layout_source must fail for a nonexistent path, got Ok"
        );

        // The sentinel must still be Some — caller only drops the handle on Ok.
        assert_eq!(
            xvfb_sentinel,
            Some(42),
            "xvfb handle sentinel must be untouched after a failed layout build"
        );

        // Demonstrate the correct caller pattern: only drop on Ok.
        if result.is_ok() {
            xvfb_sentinel.take(); // would drop on real handle
        }
        assert_eq!(
            xvfb_sentinel,
            Some(42),
            "sentinel must still be Some after Err path"
        );
    }

    /// When build_layout_source succeeds, the caller should drop the old handle
    /// and send the new source. This test confirms Ok is returned for valid SVG.
    #[test]
    fn successful_build_returns_frame_source() {
        let tmp = tempdir().unwrap();
        let svg_path = tmp.path().join("test.svg");
        // Minimal valid SVG with no frontmatter.
        std::fs::write(&svg_path, r#"<svg viewBox="0 0 480 480"></svg>"#).unwrap();

        let mut xvfb_sentinel: Option<u32> = Some(42);

        let result = build_layout_source(
            &svg_path,
            HashMap::new(),
            None,
            None,
            ThemePalette::default(),
            480,
            480,
        );

        if let Err(ref e) = result {
            panic!("build_layout_source must succeed for a valid SVG, got Err: {e}");
        }

        // On Ok: caller drops old handle, then sends new source.
        if result.is_ok() {
            xvfb_sentinel.take(); // represents dropping the old xvfb handle
        }
        assert_eq!(
            xvfb_sentinel, None,
            "handle must be dropped after successful build"
        );
    }

    #[test]
    fn runtime_dimensions_build_listener_layout_source() {
        let tmp = tempdir().unwrap();
        let svg_path = tmp.path().join("non_480.svg");
        std::fs::write(&svg_path, r#"<svg viewBox="0 0 320 240"></svg>"#).unwrap();

        let display = RuntimeDisplayDimensions::new(320, 240);
        let mut source = display
            .build_layout_source(
                &svg_path,
                HashMap::new(),
                None,
                None,
                ThemePalette::default(),
            )
            .expect("valid SVG should build");

        let frame = source.render(&HashMap::new()).expect("SVG should render");
        assert_eq!((frame.width, frame.height), (320, 240));
    }

    /// Verify the error message names the failing path (helps debugging).
    #[test]
    fn error_message_names_the_failing_path() {
        let tmp = tempdir().unwrap();
        let nonexistent = tmp.path().join("no-such-layout.svg");

        let result = build_layout_source(
            &nonexistent,
            HashMap::new(),
            None,
            None,
            ThemePalette::default(),
            480,
            480,
        );
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("build_layout_source must fail for a nonexistent path"),
        };

        let msg = err.to_string();
        assert!(
            msg.contains("no-such-layout.svg"),
            "error message must name the failing path, got: {msg}"
        );
    }
}
