export const ErrorCode = Object.freeze({
  NotOwner: "not_owner",
  InvalidRequest: "invalid_request",
  InvalidTopic: "invalid_topic",
  InvalidKey: "invalid_key",
  InvalidMeta: "invalid_meta",
  KeyTooLong: "key_too_long",
  TopicTooLong: "topic_too_long",
  HolderTooLong: "holder_too_long",
  Gone: "gone",
  NotFound: "not_found",
  AlreadyTracked: "already_tracked",
  Conflict: "conflict",
  OperationConflict: "operation_conflict",
  SequenceConflict: "sequence_conflict",
  RetryExpired: "retry_expired",
  HolderLimit: "holder_limit",
  TopicLimit: "topic_limit",
  IdentityCapacityExceeded: "identity_capacity_exceeded",
  MetaTooLarge: "meta_too_large",
  NotReady: "not_ready",
  Unavailable: "unavailable",
  Overloaded: "overloaded",
  GenerationChanged: "generation_changed",
  BarrierExpired: "barrier_expired",
  SnapshotTooLarge: "snapshot_too_large",
  HookRejected: "hook_rejected",
  HookUnavailable: "hook_unavailable",
  TokenMissing: "token_missing",
  TokenMalformed: "token_malformed",
  TokenAlgorithm: "token_algorithm",
  TokenUnknownKey: "token_unknown_key",
  TokenSignature: "token_signature",
  TokenLifetime: "token_lifetime",
  TokenNotYetValid: "token_not_yet_valid",
  TokenExpired: "token_expired",
  TokenEncode: "token_encode",
  SessionExhausted: "session_exhausted",
});

export class PresenceError extends Error {
  /**
   * @param {string} code one of ErrorCode's values
   * @param {string} message
   * @param {{ topic?: string, cause?: unknown }} [options]
   */
  constructor(code, message, options = {}) {
    super(message, options.cause === undefined ? undefined : { cause: options.cause });
    this.code = code;
    this.topic = options.topic;
  }
}

/** A per-topic join denial, surfaced the way phoenix.js surfaces a channel join error. */
export class JoinError extends PresenceError {
  /**
   * @param {string} topic
   * @param {string} code
   * @param {string} message
   */
  constructor(topic, code, message) {
    super(code, message, { topic });
  }
}
