use std::time::Duration;
use tempfile::NamedTempFile;
use thermalwriter::render::components::animation::AnimationSource;

/// Create a minimal 2-frame 2x2 GIF for testing.
fn make_test_gif() -> NamedTempFile {
    use image::{GrayImage, codecs::gif::{GifEncoder, Repeat}, Frame};

    let mut tmp = NamedTempFile::new().unwrap();

    {
        let mut encoder = GifEncoder::new(&mut tmp);
        encoder.set_repeat(Repeat::Infinite).unwrap();

        // Frame 1: all white pixels, 100ms delay
        let img1 = GrayImage::from_pixel(2, 2, image::Luma([255u8]));
        let frame1 = Frame::from_parts(
            image::DynamicImage::ImageLuma8(img1).into_rgba8(),
            0, 0,
            image::Delay::from_numer_denom_ms(100, 1),
        );
        encoder.encode_frame(frame1).unwrap();

        // Frame 2: all black pixels, 100ms delay
        let img2 = GrayImage::from_pixel(2, 2, image::Luma([0u8]));
        let frame2 = Frame::from_parts(
            image::DynamicImage::ImageLuma8(img2).into_rgba8(),
            0, 0,
            image::Delay::from_numer_denom_ms(100, 1),
        );
        encoder.encode_frame(frame2).unwrap();
    }

    tmp
}

#[test]
fn animation_loads_gif_and_returns_frames() {
    let tmp = make_test_gif();
    let anim = AnimationSource::load(tmp.path()).expect("Should load GIF");
    assert_eq!(anim.frame_count(), 2, "Should have 2 frames");
}

#[test]
fn animation_frame_at_returns_first_frame_at_zero() {
    let tmp = make_test_gif();
    let anim = AnimationSource::load(tmp.path()).expect("Should load GIF");
    let frame = anim.frame_at(Duration::ZERO);
    assert!(frame.is_some(), "Should return frame at elapsed=0");
    let pixels = frame.unwrap();
    // Frame 1 is all white — first pixel R channel should be 255
    assert_eq!(pixels[0], 255, "First frame should have white pixels");
}

#[test]
fn animation_frame_at_loops_back_to_start() {
    let tmp = make_test_gif();
    let anim = AnimationSource::load(tmp.path()).expect("Should load GIF");
    let total = anim.total_duration();
    // After one full loop, should return to frame 1 (white)
    let frame = anim.frame_at(total + Duration::from_millis(10));
    assert!(frame.is_some(), "Should return frame after looping");
    let pixels = frame.unwrap();
    assert_eq!(pixels[0], 255, "After loop, first frame should be white again");
}

#[test]
fn animation_native_fps_is_reasonable() {
    let tmp = make_test_gif();
    let anim = AnimationSource::load(tmp.path()).expect("Should load GIF");
    let fps = anim.native_fps();
    // 100ms per frame = 10fps
    assert!((fps - 10.0).abs() < 1.0, "10fps expected, got {}", fps);
}

#[test]
fn animation_base64_frame_is_valid_png_data() {
    let tmp = make_test_gif();
    let anim = AnimationSource::load(tmp.path()).expect("Should load GIF");
    let b64 = anim.base64_frame_at(Duration::ZERO);
    assert!(b64.is_some(), "Should return base64 frame");
    let b64_str = b64.unwrap();
    // PNG magic bytes encode to base64 prefix "iVBORw0KGgo"
    assert!(
        b64_str.starts_with("iVBORw0KGgo"),
        "Should be valid PNG base64, got: {}",
        &b64_str[..20.min(b64_str.len())]
    );
}

#[test]
fn animation_fps_is_finite_and_positive() {
    let tmp = make_test_gif();
    let anim = AnimationSource::load(tmp.path()).expect("Should load GIF");
    let fps = anim.native_fps();
    assert!(fps.is_finite(), "fps should be finite");
    assert!(fps > 0.0, "fps should be positive");
}
