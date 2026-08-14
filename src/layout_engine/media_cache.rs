//! Bounded, deterministic local-media resolution for layout scenes.
//!
//! Media modules emit logical image sources.  This cache resolves those sources
//! once per content fingerprint and native surface profile, retaining straight
//! RGBA8 pixels for the scene backend.  It deliberately does not own a frame
//! buffer or introduce another transport format.

use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::{GenericImageView, ImageReader, Limits};

use super::diagnostic::{DiagnosticSeverity, LayoutDiagnostic};
use super::surface::{DisplaySurfaceProfile, SurfaceProfileId};
use super::svg_backend::MediaAsset;

/// Stable diagnostic code for bounded media-cache failures.
pub const MEDIA_CACHE_DIAGNOSTIC_CODE: &str = "TWLAYOUT-E031";

/// Maximum encoded file size accepted before attempting to decode it.
pub const MAX_MEDIA_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Maximum decoded width or height accepted for one media source.
pub const MAX_MEDIA_DIMENSION: u32 = 8192;

/// Maximum decoded RGBA allocation retained for one media source.
pub const MAX_MEDIA_ALLOC_BYTES: u64 = 256 * 1024 * 1024;

/// The key used for decoded-media reuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MediaCacheKey {
    /// Fingerprint of the complete encoded source bytes.
    pub content_fingerprint: u64,
    /// Native profile width used for this render context.
    pub profile_width: u32,
    /// Native profile height used for this render context.
    pub profile_height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CacheContext {
    document_fingerprint: u64,
    profile_id: SurfaceProfileId,
    profile_width: u32,
    profile_height: u32,
}

/// Decoded local image assets keyed by source content and target profile.
#[derive(Debug)]
pub struct MediaCache {
    media_root: PathBuf,
    context: Option<CacheContext>,
    entries: HashMap<MediaCacheKey, MediaAsset>,
    source_keys: HashMap<(PathBuf, u32, u32), MediaCacheKey>,
}

impl MediaCache {
    /// Create an empty cache rooted at the approved local-media directory.
    pub fn new(media_root: impl Into<PathBuf>) -> Self {
        Self {
            media_root: media_root.into(),
            context: None,
            entries: HashMap::new(),
            source_keys: HashMap::new(),
        }
    }

    /// Return a cache rooted at the current working directory when it is
    /// available.  A relative fallback keeps construction infallible; actual
    /// source resolution still returns a diagnostic if the directory is unusable.
    pub fn with_current_dir() -> Self {
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::new(root)
    }

    /// Return the approved media root used by media emitters.
    pub fn media_root(&self) -> &Path {
        &self.media_root
    }

    /// Change the approved media root and invalidate all cached assets.
    pub fn set_media_root(&mut self, media_root: impl Into<PathBuf>) {
        let media_root = media_root.into();
        if self.media_root != media_root {
            self.media_root = media_root;
            self.clear();
        }
    }

    /// Start a render context.  Draft/document or profile changes never reuse
    /// decoded assets from the prior context, even when a source path is reused.
    pub fn prepare(&mut self, document_fingerprint: u64, surface: DisplaySurfaceProfile) {
        let context = CacheContext {
            document_fingerprint,
            profile_id: surface.id,
            profile_width: surface.width,
            profile_height: surface.height,
        };
        if self.context != Some(context) {
            self.context = Some(context);
            self.clear_entries();
        }
    }

    /// Drop all decoded assets while retaining the approved media root.
    pub fn clear(&mut self) {
        self.context = None;
        self.clear_entries();
    }

    /// Number of decoded cache entries currently retained.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return whether the cache has no decoded entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Resolve a canonical local image path into straight RGBA8 pixels.
    ///
    /// File-size, image-dimension, byte-count, and allocation checks happen
    /// before the result is retained.  Every failure is returned as a stable
    /// layout diagnostic; malformed media must not cause a renderer panic.
    pub fn resolve_path(
        &mut self,
        path: &Path,
        profile_width: u32,
        profile_height: u32,
    ) -> Result<MediaAsset, LayoutDiagnostic> {
        if profile_width == 0 || profile_height == 0 {
            return Err(media_diagnostic(
                path,
                "media resolution needs a non-zero target profile",
                format!("target profile dimensions are {profile_width}x{profile_height}"),
                "Choose a bounded profile with positive native dimensions",
            ));
        }

        let metadata = fs::metadata(path).map_err(|error| {
            media_diagnostic(
                path,
                "media source could not be read",
                format!("could not stat `{}`: {error}", path.display()),
                "Choose an existing local PNG or JPEG below the approved media directory",
            )
        })?;
        let file_len = metadata.len();
        if file_len > MAX_MEDIA_FILE_BYTES {
            return Err(media_diagnostic(
                path,
                "media source exceeds the bounded file limit",
                format!(
                    "`{}` is {file_len} bytes; the maximum is {MAX_MEDIA_FILE_BYTES} bytes",
                    path.display()
                ),
                "Choose a local image no larger than 8 MB",
            ));
        }

        let bytes = fs::read(path).map_err(|error| {
            media_diagnostic(
                path,
                "media source could not be read",
                format!("could not read `{}`: {error}", path.display()),
                "Grant read access to the local media file or choose another image",
            )
        })?;
        let bytes_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if bytes_len > MAX_MEDIA_FILE_BYTES {
            return Err(media_diagnostic(
                path,
                "media source exceeds the bounded file limit",
                format!(
                    "`{}` grew to {bytes_len} bytes while it was being read",
                    path.display()
                ),
                "Choose a local image no larger than 8 MB",
            ));
        }
        let key = MediaCacheKey {
            content_fingerprint: content_fingerprint(&bytes),
            profile_width,
            profile_height,
        };
        if let Some(asset) = self.entries.get(&key).cloned() {
            self.remember_source(path, key);
            return Ok(asset);
        }

        let asset = decode_rgba8(path, &bytes)?;
        self.entries.insert(key, asset.clone());
        self.remember_source(path, key);
        Ok(asset)
    }

    fn clear_entries(&mut self) {
        self.entries.clear();
        self.source_keys.clear();
    }

    fn remember_source(&mut self, path: &Path, key: MediaCacheKey) {
        let source_key = (path.to_path_buf(), key.profile_width, key.profile_height);
        let previous = self.source_keys.insert(source_key, key);
        let Some(previous) = previous.filter(|previous| *previous != key) else {
            return;
        };
        // A content entry may be shared by several paths.  Only remove the old
        // entry when no other source still points at it.
        if !self
            .source_keys
            .values()
            .any(|candidate| *candidate == previous)
        {
            self.entries.remove(&previous);
        }
    }
}

impl Default for MediaCache {
    fn default() -> Self {
        Self::with_current_dir()
    }
}

fn decode_rgba8(path: &Path, bytes: &[u8]) -> Result<MediaAsset, LayoutDiagnostic> {
    let cursor = Cursor::new(bytes);
    let mut reader = ImageReader::new(cursor)
        .with_guessed_format()
        .map_err(|error| {
            media_diagnostic(
                path,
                "media source has an unknown image format",
                format!("could not identify `{}`: {error}", path.display()),
                "Choose a valid local PNG, JPEG, GIF, or WebP image",
            )
        })?;

    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_MEDIA_DIMENSION);
    limits.max_image_height = Some(MAX_MEDIA_DIMENSION);
    limits.max_alloc = Some(MAX_MEDIA_ALLOC_BYTES);
    reader.limits(limits);

    let decoded = reader.decode().map_err(|error| {
        media_diagnostic(
            path,
            "media source could not be decoded",
            format!("failed to decode `{}`: {error}", path.display()),
            format!(
                "Use a supported image at most {MAX_MEDIA_DIMENSION} pixels per side and within the bounded allocation"
            ),
        )
    })?;
    let (width, height) = decoded.dimensions();
    if width == 0 || height == 0 {
        return Err(media_diagnostic(
            path,
            "media source has zero dimensions",
            format!("decoded `{}` as {width}x{height}", path.display()),
            "Choose a non-empty local image",
        ));
    }
    if width > MAX_MEDIA_DIMENSION || height > MAX_MEDIA_DIMENSION {
        return Err(media_diagnostic(
            path,
            "media source exceeds the bounded decode dimensions",
            format!(
                "decoded `{}` as {width}x{height}; each side must be at most {MAX_MEDIA_DIMENSION}",
                path.display()
            ),
            format!("Resize the image to at most {MAX_MEDIA_DIMENSION} pixels per side"),
        ));
    }

    let expected = checked_rgba_len(path, width, height)?;
    let rgba = decoded.into_rgba8().into_raw();
    if rgba.len() != expected {
        return Err(media_diagnostic(
            path,
            "decoded media has an invalid RGBA buffer",
            format!(
                "decoded `{}` to {} bytes; dimensions require {expected} bytes",
                path.display(),
                rgba.len()
            ),
            "Use an image that decodes to exactly width × height × 4 RGBA8 bytes",
        ));
    }

    Ok(MediaAsset::rgba8(width, height, rgba))
}

fn checked_rgba_len(path: &Path, width: u32, height: u32) -> Result<usize, LayoutDiagnostic> {
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| {
            media_diagnostic(
                path,
                "media allocation size overflowed",
                format!("could not calculate RGBA8 bytes for {width}x{height}"),
                "Choose an image with bounded native dimensions",
            )
        })?;
    if bytes > MAX_MEDIA_ALLOC_BYTES {
        return Err(media_diagnostic(
            path,
            "media source exceeds the bounded allocation",
            format!(
                "decoded `{}` needs {bytes} bytes; the maximum is {MAX_MEDIA_ALLOC_BYTES} bytes",
                path.display()
            ),
            "Resize the image so its decoded RGBA8 allocation stays within 256 MiB",
        ));
    }
    usize::try_from(bytes).map_err(|_| {
        media_diagnostic(
            path,
            "media allocation size is not representable",
            format!("decoded `{}` needs {bytes} bytes", path.display()),
            "Choose a smaller local image",
        )
    })
}

fn content_fingerprint(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn media_diagnostic(
    path: &Path,
    message: impl Into<String>,
    reason: impl Into<String>,
    fix: impl Into<String>,
) -> LayoutDiagnostic {
    let mut diagnostic = LayoutDiagnostic::new(
        MEDIA_CACHE_DIAGNOSTIC_CODE,
        DiagnosticSeverity::Error,
        message,
        reason,
        fix,
    );
    diagnostic.property_path = Some(format!("media.source ({})", path.display()));
    diagnostic
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn tiny_png(color: [u8; 4]) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(2, 2, image::Rgba(color));
        let mut output = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut output, image::ImageFormat::Png)
            .expect("fixture PNG");
        output.into_inner()
    }

    #[test]
    fn resolves_and_reuses_decoded_media_by_content_and_profile() {
        let directory = tempdir().expect("fixture directory");
        let path = directory.path().join("wallpaper.png");
        fs::write(&path, tiny_png([0x11, 0x22, 0x33, 0xff])).expect("fixture image");
        let surface =
            super::super::surface::rectangular_surface_profile(480, 480).expect("fixture surface");
        let mut cache = MediaCache::new(directory.path());
        cache.prepare(1, *surface);

        let first = cache.resolve_path(&path, 480, 480).expect("decode fixture");
        let second = cache.resolve_path(&path, 480, 480).expect("reuse fixture");
        assert_eq!(first, second);
        assert_eq!(cache.len(), 1);

        let _different_profile = cache
            .resolve_path(&path, 1280, 480)
            .expect("decode second profile");
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn changed_content_replaces_the_source_entry() {
        let directory = tempdir().expect("fixture directory");
        let path = directory.path().join("wallpaper.png");
        fs::write(&path, tiny_png([0x11, 0x22, 0x33, 0xff])).expect("fixture image");
        let surface =
            super::super::surface::rectangular_surface_profile(480, 480).expect("fixture surface");
        let mut cache = MediaCache::new(directory.path());
        cache.prepare(1, *surface);
        let first = cache.resolve_path(&path, 480, 480).expect("first decode");

        fs::write(&path, tiny_png([0xaa, 0xbb, 0xcc, 0xff])).expect("changed image");
        let second = cache.resolve_path(&path, 480, 480).expect("second decode");
        assert_ne!(first, second);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn oversized_files_return_diagnostics_without_decoding() {
        let directory = tempdir().expect("fixture directory");
        let path = directory.path().join("too-large.png");
        let file = vec![0u8; (MAX_MEDIA_FILE_BYTES + 1) as usize];
        fs::write(&path, file).expect("oversized fixture");
        let mut cache = MediaCache::new(directory.path());
        let error = cache
            .resolve_path(&path, 480, 480)
            .expect_err("oversized media must be rejected");
        assert_eq!(error.code, MEDIA_CACHE_DIAGNOSTIC_CODE);
        assert!(error.reason.contains("maximum"));
    }
}
