//! Frames end to end: typed value → bytes → decoder → typed value.
//!
//! The unit tests in `src/rpc.rs` cover each piece; these prove the pieces compose for every
//! method the protocol declares, which is what the daemon will actually do in M1.

mod support;

use serde_json::Value;

use hatchery_protocol::{
    FrameDecoder, FrameError, Incoming, Notification, Request, Response, classify, decode_frame,
    encode_frame, method,
};

use support::{frames, method_params};

#[test]
fn a_null_id_is_not_taken_for_a_correlation() {
    // JSON-RPC 2.0 lets a peer answer a request it could not identify with `"id": null`. Every
    // hatchery id is minted before its request is written, so such a frame is junk rather than a
    // reply we lost — `classify` refuses it instead of inventing a correlation (see its docs).
    let value = serde_json::json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": {"code": -32600, "message": "invalid request"},
    });
    assert_eq!(
        classify(value.clone()),
        Err(FrameError::NullId { method: None })
    );

    // The dangerous spelling: an explicit null on a frame that names a method. Read as "no id"
    // it becomes a notification, and the caller of `session/prompt` waits forever for an answer
    // nothing is going to send.
    let downgraded = serde_json::json!({
        "jsonrpc": "2.0",
        "id": null,
        "method": "session/prompt",
        "params": {"session_id": "s", "text": "hi"},
    });
    assert_eq!(
        classify(downgraded),
        Err(FrameError::NullId {
            method: Some("session/prompt".to_owned())
        })
    );

    // The same frame arriving on the transport: reported, and the stream stays usable.
    let frame = encode_frame(&value).expect("encode");
    let mut decoder = FrameDecoder::default();
    let lines = decoder.push(frame.as_bytes()).expect("push");
    assert_eq!(lines.len(), 1);
    assert_eq!(
        decode_frame(&lines[0]),
        Err(FrameError::NullId { method: None })
    );
    let after = decoder
        .push(
            encode_frame(&Request::new(1_i64, method::SESSION_CANCEL))
                .expect("encode")
                .as_bytes(),
        )
        .expect("the next frame still decodes");
    assert_eq!(after.len(), 1);
}

#[test]
fn every_methods_params_survive_a_real_frame() {
    for (name, params) in method_params() {
        let request = Request::new("call-1", name)
            .with_params(&params)
            .unwrap_or_else(|error| panic!("{name} params are not representable: {error}"));
        let frame = encode_frame(&request).expect("encode");

        // Fed one byte at a time on purpose: a transport hands over arbitrary chunks, and a
        // decoder that only works on whole frames would pass a simpler test and fail in UDS.
        let mut decoder = FrameDecoder::default();
        let mut lines = Vec::new();
        for byte in frame.as_bytes() {
            lines.extend(
                decoder
                    .push(&[*byte])
                    .unwrap_or_else(|error| panic!("{name} byte-wise push failed: {error}")),
            );
        }
        assert_eq!(lines.len(), 1, "{name} produced {} frames", lines.len());
        assert_eq!(decoder.buffered_bytes(), 0, "{name} left bytes behind");

        match decode_frame(&lines[0]).unwrap_or_else(|error| panic!("{name}: {error}")) {
            Incoming::Request(back) => {
                assert_eq!(back.method, name);
                assert_eq!(back.id, hatchery_protocol::Id::String("call-1".to_owned()));
                assert_eq!(
                    back.params_as::<Value>().expect("params survive framing"),
                    params,
                    "{name} parameters changed in transit"
                );
            }
            other => panic!("{name} came back as {other:?}"),
        }
    }
}

#[test]
fn every_frame_kind_classifies_as_itself_and_survives_re_encoding() {
    for (stem, value) in frames() {
        let incoming = classify(value.clone())
            .unwrap_or_else(|error| panic!("{stem} does not classify: {error}"));
        assert!(
            matches!(
                (stem, &incoming),
                ("frame_request", Incoming::Request(_))
                    | ("frame_notification", Incoming::Notification(_))
                    | (
                        "frame_response_ok" | "frame_response_err",
                        Incoming::Response(_)
                    )
            ),
            "{stem} classified as {incoming:?}"
        );

        // Re-encode and decode: the frame must mean the same thing after a round trip.
        let frame = encode_frame(&value).expect("encode");
        let again = decode_frame(&frame).unwrap_or_else(|error| panic!("{stem}: {error}"));
        assert_eq!(
            again.expects_response(),
            incoming.expects_response(),
            "{stem} changed kind after a round trip"
        );
        assert_eq!(again.method(), incoming.method(), "{stem} lost its method");
    }
}

#[test]
fn the_frames_set_covers_every_frame_kind() {
    let stems: Vec<&str> = frames().iter().map(|(stem, _)| *stem).collect();
    assert_eq!(
        stems,
        vec![
            "frame_request",
            "frame_notification",
            "frame_response_ok",
            "frame_response_err"
        ]
    );
}

#[test]
fn a_request_says_which_method_and_carries_typed_parameters() {
    let value = frames()
        .into_iter()
        .find(|(stem, _)| *stem == "frame_request")
        .map(|(_, value)| value)
        .expect("the request frame exists");

    let incoming = classify(value).expect("classify");
    assert_eq!(incoming.method(), Some(method::SESSION_LOAD));
    match incoming {
        Incoming::Request(request) => {
            let params = request
                .params_as::<method::SessionLoadParams>()
                .expect("typed parameters");
            assert_eq!(params.session_id, support::session_id());
        }
        other => panic!("expected a request, got {other:?}"),
    }
}

#[test]
fn several_frames_in_one_read_arrive_in_order() {
    let requests = [
        Request::new(1_i64, method::SESSION_CANCEL),
        Request::new(2_i64, method::SESSION_LOAD),
        Request::new(3_i64, method::SESSION_LIST),
    ];
    let mut stream = String::new();
    for request in &requests {
        stream.push_str(&encode_frame(request).expect("encode"));
    }
    let notification = Notification::new(method::DAEMON_HELLO);
    stream.push_str(&encode_frame(&notification).expect("encode"));
    let response = Response::ok(4_i64, &serde_json::json!({})).expect("encode result");
    stream.push_str(&encode_frame(&response).expect("encode"));

    let mut decoder = FrameDecoder::default();
    let lines = decoder.push(stream.as_bytes()).expect("push");
    assert_eq!(lines.len(), 5, "one read must yield every complete frame");

    let methods: Vec<Option<String>> = lines
        .iter()
        .map(|line| {
            decode_frame(line)
                .expect("decode")
                .method()
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(
        methods,
        vec![
            Some(method::SESSION_CANCEL.to_owned()),
            Some(method::SESSION_LOAD.to_owned()),
            Some(method::SESSION_LIST.to_owned()),
            Some(method::DAEMON_HELLO.to_owned()),
            None
        ]
    );
}
