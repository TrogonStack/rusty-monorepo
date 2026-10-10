use std::hint::black_box;
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::{Map, Value};

wit_bindgen::generate!({
    path: "../../wit",
    world: "hooks",
});

const HOG_BYTES: usize = 256 * 1024 * 1024;
const RAW_OVERSIZED_BYTES: usize = 16 * 1024 + 1;
const ENVELOPE_OVERSIZED_BYTES: usize = 5 * 1024;

static CALLS: AtomicU32 = AtomicU32::new(0);

struct Example;

impl Guest for Example {
    fn enrich(_op: Op, topic: String, key: String, meta: Vec<u8>) -> Result<Vec<u8>, HookError> {
        let calls = CALLS.fetch_add(1, Ordering::SeqCst) + 1;
        match key.as_str() {
            "blocked" => return Err(HookError::Reject(format!("{key} may not join {topic}"))),
            "broken" => return Err(HookError::Error("lookup backend is down".to_owned())),
            "garbage" => return Ok(b"[1, 2, 3]".to_vec()),
            "slow" => loop {
                black_box(&key);
            },
            "hog" => {
                let block = black_box(vec![1u8; HOG_BYTES]);
                return Ok(format!("{{\"hog\":{}}}", block.len()).into_bytes());
            }
            "raw-oversized" => return Ok(vec![b' '; RAW_OVERSIZED_BYTES]),
            "envelope-oversized" => {
                let padding = "x".repeat(ENVELOPE_OVERSIZED_BYTES);
                return Ok(format!("{{\"padding\":\"{padding}\"}}").into_bytes());
            }
            "fresh" => return Ok(format!("{{\"calls\":{calls}}}").into_bytes()),
            "linger" => {
                for _ in 0..50_000_000u64 {
                    black_box(&key);
                }
            }
            _ => {}
        }
        let mut fields: Map<String, Value> =
            serde_json::from_slice(&meta).map_err(|err| HookError::Error(err.to_string()))?;
        fields.insert("enriched".to_owned(), Value::Bool(true));
        serde_json::to_vec(&fields).map_err(|err| HookError::Error(err.to_string()))
    }
}

export!(Example);
