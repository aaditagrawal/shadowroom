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
#[test]
fn existing_request_spellings_are_unchanged_and_flow_control_is_additive() {
    // Older agents decode exactly these spellings; flow control adds new
    // variants (sent only after the agent advertises the capability) rather
    // than new fields on existing requests.
    let photos = Request::Photos {
        root: "r".into(),
        since_revision: 3,
    };
    assert_eq!(
        serde_json::to_string(&photos).unwrap(),
        r#"{"type":"photos","root":"r","since_revision":3}"#
    );
    let hello = Request::Hello {
        protocol_version: PROTOCOL_VERSION,
        app_version: "x".into(),
    };
    assert_eq!(
        serde_json::to_string(&hello).unwrap(),
        r#"{"type":"hello","protocol_version":1,"app_version":"x"}"#
    );
    assert_eq!(
        serde_json::to_string(&Request::FlowControl {
            window: FLOW_WINDOW
        })
        .unwrap(),
        r#"{"type":"flow_control","window":256}"#
    );
    assert_eq!(
        serde_json::to_string(&Request::Credit {
            request_id: 9,
            frames: 128
        })
        .unwrap(),
        r#"{"type":"credit","request_id":9,"frames":128}"#
    );
    // Synced keeps its old spelling without a history identity, and older
    // agents' frames decode as "unknown history".
    let legacy = Response::Synced {
        revision: 3,
        history: None,
    };
    assert_eq!(
        serde_json::to_string(&legacy).unwrap(),
        r#"{"type":"synced","revision":3}"#
    );
    assert!(matches!(
        serde_json::from_str::<Response>(r#"{"type":"synced","revision":3}"#).unwrap(),
        Response::Synced {
            revision: 3,
            history: None
        }
    ));
    let current: Response =
        serde_json::from_str(r#"{"type":"synced","revision":3,"history":"h"}"#).unwrap();
    assert!(matches!(current, Response::Synced { history: Some(h), .. } if h == "h"));
    // An older client's definition ignores the new field.
    #[derive(serde::Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum OldResponse {
        Synced { revision: u64 },
    }
    let OldResponse::Synced { revision } =
        serde_json::from_str(r#"{"type":"synced","revision":3,"history":"h"}"#).unwrap();
    assert_eq!(revision, 3);
    let mut wire = Vec::new();
    write_frame(
        &mut wire,
        &Frame::new(
            5,
            true,
            Request::Credit {
                request_id: 4,
                frames: 1,
            },
            vec![],
        ),
    )
    .unwrap();
    let frame = read_frame::<Request>(&mut Cursor::new(wire))
        .unwrap()
        .unwrap();
    assert!(matches!(
        frame.header.message,
        Request::Credit {
            request_id: 4,
            frames: 1
        }
    ));
}
