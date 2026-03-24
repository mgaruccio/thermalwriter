//! Xvfb framebuffer capture: reads pixels from an mmap'd XWD file produced by Xvfb -fbdir.

use std::fs::File;
use std::path::Path;
use anyhow::{Context, Result, bail};

use super::{FrameSource, RawFrame, SensorData};

/// XWD header field offsets (all big-endian u32).
const XWD_HEADER_SIZE: usize = 0;
const XWD_PIXMAP_WIDTH: usize = 16;
const XWD_PIXMAP_HEIGHT: usize = 20;
const XWD_BITS_PER_PIXEL: usize = 44;
const XWD_BYTES_PER_LINE: usize = 48;
const XWD_NCOLORS: usize = 76;
const XWD_COLOR_ENTRY_SIZE: usize = 12;

/// Read a big-endian u32 from a byte slice at the given offset.
fn read_be_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]])
}

/// Parsed XWD header info needed for pixel extraction.
struct XwdHeader {
    pixel_data_offset: usize,
    width: u32,
    height: u32,
    bytes_per_line: u32,
}

fn parse_xwd_header(data: &[u8]) -> Result<XwdHeader> {
    if data.len() < 100 {
        bail!("XWD file too small: {} bytes", data.len());
    }

    let header_size = read_be_u32(data, XWD_HEADER_SIZE) as usize;
    let width = read_be_u32(data, XWD_PIXMAP_WIDTH);
    let height = read_be_u32(data, XWD_PIXMAP_HEIGHT);
    let bits_per_pixel = read_be_u32(data, XWD_BITS_PER_PIXEL);
    let bytes_per_line = read_be_u32(data, XWD_BYTES_PER_LINE);
    let ncolors = read_be_u32(data, XWD_NCOLORS) as usize;

    let pixel_data_offset = header_size + ncolors * XWD_COLOR_ENTRY_SIZE;

    if bits_per_pixel != 32 {
        bail!("Unsupported XWD bits_per_pixel: {} (expected 32)", bits_per_pixel);
    }

    Ok(XwdHeader {
        pixel_data_offset,
        width,
        height,
        bytes_per_line,
    })
}

/// Convert BGRX pixel data (4 bytes/pixel, x86 byte order) to RGB (3 bytes/pixel).
fn bgrx_to_rgb(src: &[u8], width: u32, height: u32, bytes_per_line: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let bpl = bytes_per_line as usize;
    let mut rgb = Vec::with_capacity(w * h * 3);

    for y in 0..h {
        let row_start = y * bpl;
        for x in 0..w {
            let px = row_start + x * 4;
            // x86 LSBFirst: memory layout is [B, G, R, X]
            rgb.push(src[px + 2]); // R
            rgb.push(src[px + 1]); // G
            rgb.push(src[px]);     // B
        }
    }

    rgb
}

/// Frame source that captures from an Xvfb framebuffer via mmap.
pub struct XvfbSource {
    mmap: memmap2::Mmap,
    header: XwdHeader,
    expected_width: u32,
    expected_height: u32,
}

impl XvfbSource {
    /// Create a new XvfbSource from an Xvfb fbdir screen file.
    ///
    /// The file is typically `{fbdir}/Xvfb_screen0`, created by `Xvfb -fbdir`.
    pub fn new(fbdir_file: &Path, expected_width: u32, expected_height: u32) -> Result<Self> {
        let file = File::open(fbdir_file)
            .with_context(|| format!("Failed to open XWD file: {}", fbdir_file.display()))?;

        // Safety: Xvfb owns the file and writes valid data. We read-only mmap.
        let mmap = unsafe { memmap2::Mmap::map(&file) }
            .context("Failed to mmap XWD file")?;

        let header = parse_xwd_header(&mmap)?;

        if header.width != expected_width || header.height != expected_height {
            bail!(
                "XWD dimensions {}x{} don't match expected {}x{}",
                header.width, header.height, expected_width, expected_height
            );
        }

        Ok(Self {
            mmap,
            header,
            expected_width,
            expected_height,
        })
    }
}

impl FrameSource for XvfbSource {
    fn render(&mut self, _sensors: &SensorData) -> Result<RawFrame> {
        let pixel_data = &self.mmap[self.header.pixel_data_offset..];
        let rgb = bgrx_to_rgb(
            pixel_data,
            self.header.width,
            self.header.height,
            self.header.bytes_per_line,
        );

        Ok(RawFrame {
            data: rgb,
            width: self.expected_width,
            height: self.expected_height,
        })
    }

    fn name(&self) -> &str {
        "xvfb"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal synthetic XWD file for testing.
    fn build_test_xwd(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
        let header_size: u32 = 100 + 1; // 100-byte fixed header + 1-byte NUL window name
        let bits_per_pixel: u32 = 32;
        let bytes_per_line = width * 4;
        let ncolors: u32 = 0;

        let mut buf = vec![0u8; header_size as usize + pixels.len()];

        // Write header fields (big-endian)
        buf[0..4].copy_from_slice(&header_size.to_be_bytes());
        buf[4..8].copy_from_slice(&7u32.to_be_bytes()); // file_version = 7
        buf[8..12].copy_from_slice(&2u32.to_be_bytes()); // pixmap_format = ZPixmap
        buf[12..16].copy_from_slice(&24u32.to_be_bytes()); // pixmap_depth
        buf[16..20].copy_from_slice(&width.to_be_bytes());
        buf[20..24].copy_from_slice(&height.to_be_bytes());
        buf[44..48].copy_from_slice(&bits_per_pixel.to_be_bytes());
        buf[48..52].copy_from_slice(&bytes_per_line.to_be_bytes());
        buf[76..80].copy_from_slice(&ncolors.to_be_bytes());

        // Copy pixel data after header
        buf[header_size as usize..].copy_from_slice(pixels);

        buf
    }

    #[test]
    fn parse_xwd_header_extracts_dimensions() {
        let pixels = vec![0u8; 2 * 2 * 4]; // 2x2, 4 bytes/pixel
        let xwd = build_test_xwd(2, 2, &pixels);
        let header = parse_xwd_header(&xwd).unwrap();
        assert_eq!(header.width, 2);
        assert_eq!(header.height, 2);
        assert_eq!(header.bytes_per_line, 8); // width * 4
    }

    #[test]
    fn bgrx_to_rgb_converts_correctly() {
        // One pixel: B=0x11, G=0x22, R=0x33, X=0x00
        let bgrx = vec![0x11, 0x22, 0x33, 0x00];
        let rgb = bgrx_to_rgb(&bgrx, 1, 1, 4);
        assert_eq!(rgb, vec![0x33, 0x22, 0x11]); // R, G, B
    }

    #[test]
    fn bgrx_to_rgb_handles_multiple_pixels() {
        // 2x1: red pixel, blue pixel
        let bgrx = vec![
            0x00, 0x00, 0xFF, 0x00, // B=0, G=0, R=255 → red
            0xFF, 0x00, 0x00, 0x00, // B=255, G=0, R=0 → blue
        ];
        let rgb = bgrx_to_rgb(&bgrx, 2, 1, 8);
        assert_eq!(rgb, vec![0xFF, 0x00, 0x00, 0x00, 0x00, 0xFF]);
    }

    #[test]
    fn xvfb_source_reads_from_synthetic_xwd() {
        use std::io::Write;
        let width = 2u32;
        let height = 2u32;
        // 4 pixels, all green: B=0, G=255, R=0, X=0
        let pixels = vec![0x00, 0xFF, 0x00, 0x00].repeat(4);
        let xwd_data = build_test_xwd(width, height, &pixels);

        // Write to temp file
        let tmp = std::env::temp_dir().join("thermalwriter_test_xvfb.xwd");
        let mut f = std::fs::File::create(&tmp).unwrap();
        f.write_all(&xwd_data).unwrap();
        f.flush().unwrap();

        let mut source = XvfbSource::new(&tmp, width, height).unwrap();
        let frame = source.render(&std::collections::HashMap::new()).unwrap();

        assert_eq!(frame.width, 2);
        assert_eq!(frame.height, 2);
        assert_eq!(frame.data.len(), 2 * 2 * 3);
        // All pixels should be green (R=0, G=255, B=0)
        for chunk in frame.data.chunks(3) {
            assert_eq!(chunk, &[0x00, 0xFF, 0x00]);
        }

        std::fs::remove_file(&tmp).ok();
    }
}
