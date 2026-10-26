use shadowroom::remote::protocol::*;
use std::io::{self, Cursor, Read};
struct Partial<R>(R);
impl<R: Read> Read for Partial<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = buf.len().min(1);
        self.0.read(&mut buf[..len])
    }
}
#[test]
fn frames_survive_partial_reads_and_binary_payload() {
    let bytes = vec![0, 255, b'{', b'\n', 1];
    let mut wire = Vec::new();
    write_frame(
        &mut wire,
        &Frame::new(42, true, Response::Range, bytes.clone()),
    )
    .unwrap();
    write_frame(&mut wire, &Frame::new(43, true, Response::Pong, vec![])).unwrap();
    let mut reader = Partial(Cursor::new(wire));
    let frame = read_frame::<Response>(&mut reader).unwrap().unwrap();
    assert_eq!(frame.header.id, 42);
    assert_eq!(frame.payload, bytes);
    assert_eq!(
        read_frame::<Response>(&mut reader)
            .unwrap()
            .unwrap()
            .header
            .id,
        43
    );
    assert!(read_frame::<Response>(&mut reader).unwrap().is_none());
}
#[test]
fn oversized_truncated_and_inconsistent_frames_rejected() {
    assert!(
        read_frame::<Request>(&mut Cursor::new(((MAX_FRAME + 1) as u32).to_le_bytes())).is_err()
    );
    assert!(read_frame::<Request>(&mut Cursor::new([1u8, 2])).is_err());
    let mut wire = Vec::new();
    write_frame(
        &mut wire,
        &Frame::new(1, true, Response::Range, vec![1, 2, 3]),
    )
    .unwrap();
    assert!(read_frame::<Response>(&mut Cursor::new(&wire[..wire.len() - 1])).is_err());
    let json = br#"{"id":1,"done":true,"payload_len":5,"message":{"type":"ping"}}"#;
    let mut wire = Vec::new();
    wire.extend_from_slice(&((json.len() + 4) as u32).to_le_bytes());
    wire.extend_from_slice(&(json.len() as u32).to_le_bytes());
    wire.extend_from_slice(json);
    assert!(read_frame::<Request>(&mut Cursor::new(wire)).is_err());
    assert!(
        write_frame(
            &mut Vec::new(),
            &Frame::new(1, true, Response::Range, vec![0; MAX_PAYLOAD + 1])
        )
        .is_err()
    );
}
