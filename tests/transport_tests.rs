use thermalrighter::transport::bulk_usb;

#[test]
fn handshake_payload_is_64_bytes() {
    let payload = bulk_usb::handshake_payload();
    assert_eq!(payload.len(), 64);
    assert_eq!(payload[0], 0x12);
    assert_eq!(payload[1], 0x34);
    assert_eq!(payload[2], 0x56);
    assert_eq!(payload[3], 0x78);
    assert_eq!(payload[56], 0x01);
    // All other bytes are zero
    for i in 4..56 {
        assert_eq!(payload[i], 0x00, "byte {} should be 0x00", i);
    }
}

#[test]
fn frame_header_is_64_bytes_with_correct_fields() {
    let header = bulk_usb::build_frame_header(2, 480, 480, 12345);
    assert_eq!(header.len(), 64);
    // Magic
    assert_eq!(&header[0..4], &[0x12, 0x34, 0x56, 0x78]);
    // cmd = 2 (JPEG), little-endian u32
    assert_eq!(&header[4..8], &2u32.to_le_bytes());
    // width = 480
    assert_eq!(&header[8..12], &480u32.to_le_bytes());
    // height = 480
    assert_eq!(&header[12..16], &480u32.to_le_bytes());
    // mode = 2 at offset 56
    assert_eq!(&header[56..60], &2u32.to_le_bytes());
    // payload length = 12345 at offset 60
    assert_eq!(&header[60..64], &12345u32.to_le_bytes());
}
