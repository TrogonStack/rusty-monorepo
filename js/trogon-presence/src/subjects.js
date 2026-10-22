const DEFAULT_PREFIX = "presence.v1";

/** The literal shard token for the list/get initial redirect path. */
export const REDIRECT_SHARD_TOKEN = "_";

/** The custom inbox prefix every connection authenticates with, matching `trogon_presence_service::inbox::CALLER_INBOX_PREFIX`. */
export const CALLER_INBOX_PREFIX = "_INBOX_U";

/** @param {import("./values.js").PresenceKey} key @param {import("./values.js").ConnectionId} connectionId */
export function connectionInboxPrefix(key, connectionId) {
  return `${CALLER_INBOX_PREFIX}.${key.token}.${connectionId}`;
}

/**
 * Builds the canonical presence.v1 subjects from the build contract's service
 * surface table. T is a Topic's escaped tokens, K a PresenceKey's escaped token,
 * S a shard token (a real `sNN` token or the `_` redirect path), and C a
 * ConnectionId.
 */
export class Subjects {
  /** @param {string} [prefix] */
  constructor(prefix = DEFAULT_PREFIX) {
    this.prefix = prefix;
  }

  /** @param {import("./values.js").PresenceKey} key @param {import("./values.js").Topic} topic */
  trackSubject(key, topic) {
    return `${this.prefix}.track.${key.token}.${topic.tokens}`;
  }

  /** @param {import("./values.js").PresenceKey} key @param {import("./values.js").Topic} topic */
  updateSubject(key, topic) {
    return `${this.prefix}.update.${key.token}.${topic.tokens}`;
  }

  /** @param {import("./values.js").PresenceKey} key @param {import("./values.js").Topic} topic */
  untrackSubject(key, topic) {
    return `${this.prefix}.untrack.${key.token}.${topic.tokens}`;
  }

  /** @param {import("./values.js").PresenceKey} key */
  heartbeatSubject(key) {
    return `${this.prefix}.heartbeat.${key.token}`;
  }

  /** @param {import("./values.js").PresenceKey} key */
  releaseSubject(key) {
    return `${this.prefix}.release.${key.token}`;
  }

  /** @param {string} shardToken @param {import("./values.js").PresenceKey} key @param {import("./values.js").Topic} topic */
  listSubject(shardToken, key, topic) {
    return `${this.prefix}.list.${shardToken}.${key.token}.${topic.tokens}`;
  }

  /** @param {string} shardToken @param {import("./values.js").PresenceKey} key @param {import("./values.js").Topic} topic */
  getSubject(shardToken, key, topic) {
    return `${this.prefix}.get.${shardToken}.${key.token}.${topic.tokens}`;
  }

  /**
   * @param {string} shardToken
   * @param {import("./values.js").PresenceKey} key
   * @param {import("./values.js").ConnectionId} connectionId
   * @param {import("./values.js").Topic} topic
   */
  snapshotSubject(shardToken, key, connectionId, topic) {
    return `${this.prefix}.snapshot.${shardToken}.${key.token}.${connectionId}.${topic.tokens}`;
  }

  /**
   * @param {import("./values.js").PresenceKey} key
   * @param {import("./values.js").ConnectionId} connectionId
   * @param {string} snapshotId
   */
  snapshotReplySubject(key, connectionId, snapshotId) {
    return `${this.prefix}.snapshot-reply.${key.token}.${connectionId}.${snapshotId}`;
  }

  /**
   * Subscription wildcard covering every snapshot reply for one connection, matching
   * the granted snapshot-reply.K.C.* permission (the trailing token is always exactly
   * one SnapshotId, never a multi-token tail).
   * @param {import("./values.js").PresenceKey} key
   * @param {import("./values.js").ConnectionId} connectionId
   */
  snapshotReplyWildcard(key, connectionId) {
    return `${this.prefix}.snapshot-reply.${key.token}.${connectionId}.*`;
  }

  /**
   * The per-request reply-inbox subject a reader publishes a snapshot request with,
   * under the granted `_INBOX_U.K.C.>` permission.
   * @param {import("./values.js").PresenceKey} key
   * @param {import("./values.js").ConnectionId} connectionId
   * @param {string} localViewId
   * @param {string} requestId
   */
  inboxReplySubject(key, connectionId, localViewId, requestId) {
    return `${connectionInboxPrefix(key, connectionId)}.${localViewId}.${requestId}`;
  }

  /**
   * The subscription wildcard covering every request reply under one connection's
   * local view.
   * @param {import("./values.js").PresenceKey} key
   * @param {import("./values.js").ConnectionId} connectionId
   * @param {string} localViewId
   */
  inboxWildcard(key, connectionId, localViewId) {
    return `${connectionInboxPrefix(key, connectionId)}.${localViewId}.>`;
  }

  /** @param {import("./values.js").Topic} topic */
  diffSubject(topic) {
    return `${this.prefix}.diff.${topic.tokens}`;
  }

  /** @param {string} shardToken */
  epochSubject(shardToken) {
    return `${this.prefix}.epoch.${shardToken}`;
  }
}
