use anyhow::Result;
use image::{ImageBuffer, Rgb};
use thermalwriter::render::RawFrame;
use thermalwriter::transport::discovery::TransportConnector;
use thermalwriter::transport::encode::encode_frame;

fn main() -> Result<()> {
    env_logger::init();

    let connector = TransportConnector::from_config_device("auto")?;
    let (mut transport, info) = connector.connect()?;
    println!(
        "Device: {}x{} PM={} SUB={} FBL={} {} encoding={}",
        info.width(),
        info.height(),
        info.pm,
        info.sub,
        info.fbl,
        info.protocol,
        info.encoding()
    );

    let img = ImageBuffer::from_fn(info.width(), info.height(), |_x, _y| Rgb([255u8, 0u8, 0u8]));
    let frame = RawFrame {
        data: img.into_raw(),
        width: info.width(),
        height: info.height(),
    };
    let encoded = encode_frame(&frame, &info, 0, 85)?;

    for i in 0..30 {
        transport.send_frame(&encoded)?;
        println!("sent frame {i}");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    transport.close();
    Ok(())
}
