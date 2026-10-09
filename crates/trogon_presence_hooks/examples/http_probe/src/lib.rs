wit_bindgen::generate!({
    path: ["../../wit", "wit"],
    world: "trogon:presence-http-probe/http-probe",
    generate_all,
});

use wasi::http::outgoing_handler;
use wasi::http::types::{Fields, OutgoingRequest, Scheme};

struct HttpProbe;

impl Guest for HttpProbe {
    fn enrich(_op: Op, topic: String, _key: String, _meta: Vec<u8>) -> Result<Vec<u8>, HookError> {
        let request = OutgoingRequest::new(Fields::new());
        let configured = request
            .set_scheme(Some(&Scheme::Https))
            .and_then(|()| request.set_authority(Some(&format!("127.0.0.1:{topic}"))))
            .and_then(|()| request.set_path_with_query(Some("/hang")));
        if configured.is_err() {
            return Err(HookError::Error("could not build the request".to_owned()));
        }
        let pending = outgoing_handler::handle(request, None).map_err(|err| HookError::Error(format!("{err:?}")))?;
        pending.subscribe().block();
        let status = pending.get().map(|outer| outer.map(|inner| inner.map(|response| response.status())));
        Err(HookError::Error(format!("response={status:?}")))
    }
}

export!(HttpProbe);
