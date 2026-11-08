use opentelemetry::trace::Status;
use opentelemetry_semantic_conventions::attribute::{ERROR_TYPE, RPC_RESPONSE_STATUS_CODE};
use serde_json::json;

use super::*;
use crate::telemetry::testing::capture;

#[test]
fn a_refused_request_is_an_error_span() {
    let request: JsonRpcRequest<ClientRequest> = serde_json::from_value(json!({
        "jsonrpc": "2.0", "id": 4, "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "host", "version": "1" },
        },
    }))
    .expect("valid request");

    let (answer, trace) = capture(|| {
        let root = tracing::info_span!("trg");
        root.in_scope(|| refusal("weather", &request, "OAuth: not a terminal"))
    });

    trace.assert_child_of("initialize", "trg");
    let code = ErrorCode::INTERNAL_ERROR.0.to_string();
    assert_eq!(
        trace
            .attribute("initialize", RPC_RESPONSE_STATUS_CODE)
            .map(|v| v.to_string()),
        Some(code.clone())
    );
    assert_eq!(
        trace.attribute("initialize", ERROR_TYPE).map(|v| v.to_string()),
        Some(code)
    );
    assert!(matches!(
        trace.span("initialize").map(|s| &s.status),
        Some(Status::Error { .. })
    ));
    assert_eq!(
        serde_json::to_value(&answer).expect("serializable")["error"]["message"],
        json!("OAuth: not a terminal")
    );
}

struct NeverCloses;

impl Transport<RoleClient> for NeverCloses {
    type Error = std::convert::Infallible;

    fn send(
        &mut self,
        _item: TxJsonRpcMessage<RoleClient>,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send + 'static {
        std::future::ready(Ok(()))
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
        None
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        std::future::pending().await
    }
}

#[tokio::test]
async fn a_remote_that_never_closes_does_not_hold_the_proxy_open() {
    let closed = tokio::time::timeout(
        Duration::from_secs(5),
        close_remote(&mut NeverCloses, Duration::from_millis(20)),
    )
    .await;

    assert!(closed.is_ok(), "close_remote must return once the grace has passed");
}
