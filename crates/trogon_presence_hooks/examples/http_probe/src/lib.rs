wit_bindgen::generate!({
    path: ["../../wit", "wit"],
    world: "trogon:presence-http-probe/http-probe",
    generate_all,
});

use wasi::http::outgoing_handler;
use wasi::http::types::{http_error_code, Fields, IncomingResponse, OutgoingRequest, Scheme};
use wasi::io::streams::StreamError;

const READ_CHUNK: u64 = 16 * 1024;

struct HttpProbe;

struct Target {
    authority: String,
    path: String,
}

impl Target {
    fn of(topic: &str, key: &str) -> Self {
        let authority = if topic.contains(':') {
            topic.to_owned()
        } else {
            format!("127.0.0.1:{topic}")
        };
        Self {
            authority,
            path: format!("/{key}"),
        }
    }
}

fn failure(stage: &str, detail: impl core::fmt::Debug) -> HookError {
    HookError::Error(format!("{stage}={detail:?}"))
}

fn read_body(response: IncomingResponse) -> Result<Vec<u8>, HookError> {
    let body = response.consume().map_err(|()| failure("body", "unavailable"))?;
    let stream = body.stream().map_err(|()| failure("body", "unavailable"))?;
    let mut bytes = Vec::new();
    loop {
        match stream.blocking_read(READ_CHUNK) {
            Ok(chunk) => bytes.extend_from_slice(&chunk),
            Err(StreamError::Closed) => break,
            Err(StreamError::LastOperationFailed(err)) => {
                return Err(match http_error_code(&err) {
                    Some(code) => failure("body", code),
                    None => failure("body", err.to_debug_string()),
                });
            }
        }
    }
    drop(stream);
    drop(body);
    Ok(bytes)
}

impl Guest for HttpProbe {
    fn enrich(_op: Op, topic: String, key: String, _meta: Vec<u8>) -> Result<Vec<u8>, HookError> {
        let target = Target::of(&topic, &key);
        let request = OutgoingRequest::new(Fields::new());
        let configured = request
            .set_scheme(Some(&Scheme::Https))
            .and_then(|()| request.set_authority(Some(&target.authority)))
            .and_then(|()| request.set_path_with_query(Some(&target.path)));
        if configured.is_err() {
            return Err(HookError::Error("could not build the request".to_owned()));
        }
        let pending = outgoing_handler::handle(request, None).map_err(|err| failure("request", err))?;
        pending.subscribe().block();
        let response = match pending.get() {
            Some(Ok(Ok(response))) => response,
            Some(Ok(Err(code))) => return Err(failure("response", code)),
            Some(Err(())) => return Err(failure("response", "taken")),
            None => return Err(failure("response", "pending")),
        };
        let status = response.status();
        if status != 200 {
            return Err(failure("status", status));
        }
        read_body(response)
    }
}

export!(HttpProbe);
