pub const KEY_MAX_RAW_BYTES: usize = 256;
pub const KEY_MAX_ESCAPED_BYTES: usize = 768;
pub const TOPIC_SEGMENT_MAX_RAW_BYTES: usize = 128;
pub const TOPIC_SEGMENT_MAX_ESCAPED_BYTES: usize = 384;
pub const TOPIC_MAX_RAW_BYTES: usize = 256;
pub const TOPIC_MAX_ESCAPED_BYTES: usize = 768;
pub const KV_KEY_MAX_BYTES: usize = 1600;

pub const TOPIC_SEPARATOR: char = ':';
pub const TOKEN_SEPARATOR: char = '.';

pub const RANDOM_ID_BYTES: usize = 16;
pub const RANDOM_ID_ENCODED_LEN: usize = 22;
pub const PHX_REF_MAX_BYTES: usize = 64;

pub const SHARD_COUNT_MIN: u16 = 64;
pub const SHARD_COUNT_MAX: u16 = 1024;
pub const SHARD_TOKEN_PREFIX: char = 's';

pub const FNV1A64_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
pub const FNV1A64_PRIME: u64 = 0x0000_0100_0000_01b3;

pub const META_MAX_ENCODED_BYTES: usize = 4096;
pub const META_RESERVED_KEYS: [&str; 5] = ["__proto__", "constructor", "prototype", "phx_ref", "phx_ref_prev"];
pub const META_PROTOTYPE_KEYS: [&str; 3] = ["__proto__", "constructor", "prototype"];

pub const VALUE_MAX_ENCODED_BYTES: usize = 4096;

pub const ESCAPE_MARKER: u8 = b'=';
pub const EMPTY_TOKEN: &str = "=";
pub const UPPER_HEX_DIGITS: &[u8; 16] = b"0123456789ABCDEF";

pub const DEFAULT_BUCKET: &str = "PRESENCE_V1";
pub const DEFAULT_LEASE_TTL_SECS: u64 = 30;
pub const DEFAULT_HEARTBEAT_INTERVAL_SECS: u64 = 10;
pub const DEFAULT_MARKER_TTL_SECS: u64 = 300;
pub const LEASE_TTL_TO_HEARTBEAT_NUMERATOR: u32 = 5;
pub const LEASE_TTL_TO_HEARTBEAT_DENOMINATOR: u32 = 2;
pub const MARKER_TTL_TO_LEASE_TTL_FACTOR: u32 = 2;
pub const REPLICAS_MAX: u8 = 5;

pub const SHARD_COUNT_METADATA_KEY: &str = "trogon_presence.shard_count";
pub const SCHEMA_METADATA_KEY: &str = "trogon_presence.schema";
pub const GENERATION_METADATA_KEY: &str = "trogon_presence.generation";
pub const TOKEN_WIDTH_METADATA_KEY: &str = "trogon_presence.token_width";
pub const WRITER_MODE_METADATA_KEY: &str = "trogon_presence.writer_mode";
pub const FINGERPRINT_METADATA_KEY: &str = "trogon_presence.fingerprint";
pub const BUCKET_SCHEMA_V1: &str = "presence-bucket/1";
pub const BUCKET_MAX_AGE_SECS: u64 = 600;
pub const BUCKET_MAX_BYTES: i64 = 128 * 1024 * 1024;
pub const BUCKET_MAX_MESSAGE_BYTES: i32 = 512 * 1024;

pub const BATCH_MAX_MESSAGES: usize = 66;
pub const BATCH_MAX_BYTES: usize = 1024 * 1024;
pub const BATCH_SEND_BUDGET: std::time::Duration = std::time::Duration::from_secs(1);
pub const BATCH_TOTAL_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);
pub const BATCH_PROBE_STREAM_PREFIX: &str = "PRESENCE_PROBE_";
pub const BATCH_PROBE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(10 * 60);
pub const NATS_HEADER_PREAMBLE: &str = "NATS/1.0\r\n";
pub const NATS_HEADER_LINE_OVERHEAD: usize = ": \r\n".len();
pub const NATS_HEADER_TERMINATOR: &str = "\r\n";

pub const DEFAULT_GUARD_TTL_SECS: u64 = 15;
pub const KV_STREAM_PREFIX: &str = "KV_";
pub const KV_SUBJECT_PREFIX: &str = "$KV";
pub const JS_SUBJECT_ROOT: &str = "$JS";
pub const JS_API_TOKEN: &str = "API";

pub const KV_OPERATION_HEADER: &str = "KV-Operation";
pub const KV_OPERATION_PURGE: &str = "PURGE";
pub const KV_OPERATION_DELETE: &str = "DEL";
pub const ROLLUP_SUBJECT: &str = "sub";

pub const CAS_MAX_ATTEMPTS: usize = 3;
pub const TRACKER_COMMAND_BUFFER: usize = 64;

pub const HEARTBEAT_JITTER_DIVISOR: u32 = 10;
pub const HEARTBEAT_DEGRADED_AFTER_ROUNDS: u32 = 3;
pub const HEARTBEAT_PUBLISH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
pub const PRESENCE_EVENT_BUFFER: usize = 1024;

pub const CANONICAL_JSON_MAX_DEPTH: usize = 32;
pub const OPERATION_DOMAIN_TAG: &str = "trogon.presence.operation.v1";
pub const VIEW_REF_DOMAIN_TAG: &str = "trogon.presence.view-ref.v1";
pub const VIEW_REF_BYTES: usize = 16;
pub const VALUE_SCHEMA_V2: u8 = 2;
pub const RECEIPT_SCHEMA_V1: u8 = 1;
pub const GUARD_SCHEMA_V1: u8 = 1;
pub const RECEIPT_MAX_ENCODED_BYTES: usize = 4096;
pub const RECEIPT_TTL: std::time::Duration = std::time::Duration::from_secs(300);
pub const RETRY_WINDOW_MAX: std::time::Duration = std::time::Duration::from_secs(10);
pub const RETRY_WINDOW_LEASE_DIVISOR: u32 = 3;
pub const CLOCK_SKEW_ALLOWANCE: std::time::Duration = std::time::Duration::from_secs(1);
pub const CONTROL_PREFIX: &str = "ctl";
pub const CONTROL_WRITER_TOKEN: &str = "writer";
pub const CONTROL_DIRECT_TOKEN: &str = "direct";
pub const CONTROL_RECEIPT_TOKEN: &str = "receipt";
pub const HEARTBEAT_ENTRIES_PER_BATCH: usize = 64;

pub const BARRIER_WAIT_MAX: std::time::Duration = std::time::Duration::from_secs(1);
pub const BARRIER_WAITERS_PER_TOPIC: usize = 32;
pub const BARRIER_WAITERS_PER_PROCESS: usize = 128;
pub const MANAGED_HOLDER_LIMIT: usize = 32;
pub const MANAGED_TOPIC_LIMIT: usize = 64;
pub const MANAGED_GUARD_CAPACITY: usize = 512 * 1024;
pub const MANAGED_GUARD_CAPACITY_MIN: usize = 1024;
pub const MANAGED_GUARD_HEADER_ALLOWANCE: usize = 512;
pub const MANAGED_RELEASE_MAX_TARGETS: usize = BATCH_MAX_MESSAGES - 2;
pub const MANAGED_HEARTBEAT_MAX_ENTRIES: usize = HEARTBEAT_ENTRIES_PER_BATCH;
pub const GUARD_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
pub const GUARD_SELF_FENCE: std::time::Duration = std::time::Duration::from_secs(8);

pub const JETSTREAM_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
pub const DEFAULT_READ_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
pub const MIN_READ_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(100);
pub const MAX_READ_REQUEST_TIMEOUT: std::time::Duration = JETSTREAM_REQUEST_TIMEOUT;
