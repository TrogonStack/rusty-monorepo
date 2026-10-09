import { wsconnect, tokenAuthenticator } from "@nats-io/nats-core";
import { Subjects, connectionInboxPrefix } from "./subjects.js";
import { Glue } from "./glue.js";
import { TokenCache, denialFor } from "./token-cache.js";
import { tabHolderId, WebLockHolder } from "./web-lock-holder.js";
import { Topic, ConnectionId, HolderId } from "./values.js";
import { ShardCount, viewShardIndex } from "./shard.js";
import { generateRandomId } from "./ids.js";
import { frameEnvelope, HEADER_CODE, HEADER_GENERATION, HEADER_OWNER_REV, HEADER_OWNER_ID, HEADER_SEQ, header } from "./headers.js";
import { PresenceError, JoinError, ErrorCode } from "./errors.js";

const MIN_FORCED_RECONNECT_MS = 2_000;
const JOIN_DEBOUNCE_MS = 250;
const RESYNC_JITTER_MS = 300;
const MAX_AUTH_BACKOFF_MS = 30_000;
const DEFAULT_HEARTBEAT_MS = 20_000;
const COMMAND_TIMEOUT_MS = 5_000;
// Full-jitter retry (AWS-style: random(0, min(cap, base * 2^attempt))), matching the base, cap
// and deadline the service's own overload-retry tests use (crates/trogon_presence_service/tests/
// handoff_ack.rs): a shard lease handoff or writer restart is a real transient under host load,
// not a bug, and a fixed small attempt count gives up well before such a handoff settles.
const COMMAND_RETRY_ATTEMPTS = 10;
const COMMAND_RETRY_BASE_MS = 20;
const COMMAND_RETRY_CAP_MS = 500;
const COMMAND_RETRY_DEADLINE_MS = 15_000;
const SNAPSHOT_RETRY_BASE_MS = 20;
const SNAPSHOT_RETRY_CAP_MS = 500;
const SNAPSHOT_RETRY_DEADLINE_MS = 15_000;
const RETRYABLE_CODES = Object.freeze([
  ErrorCode.NotOwner,
  ErrorCode.NotReady,
  ErrorCode.Unavailable,
  ErrorCode.Overloaded,
  ErrorCode.HookUnavailable,
  ErrorCode.BarrierExpired,
]);
const META_RESERVED_KEYS = Object.freeze(["__proto__", "constructor", "prototype", "phx_ref", "phx_ref_prev"]);

function assertSafeMeta(meta) {
  for (const reserved of META_RESERVED_KEYS) {
    if (Object.hasOwn(meta, reserved)) {
      throw new PresenceError(ErrorCode.InvalidMeta, `meta must not carry the reserved key ${reserved}`);
    }
  }
}

function jittered(baseMs) {
  return Math.floor(Math.random() * baseMs);
}

/** Full-jitter backoff (AWS-style): random(0, min(cap, base * 2^attempt)). */
function fullJitterBackoff(baseMs, capMs, attempt) {
  const ceiling = Math.min(capMs, baseMs * 2 ** attempt);
  return Math.floor(Math.random() * ceiling);
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function sha256Hex(bytes) {
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** One fenced write the caller currently holds in a topic, as the service's track/update reply returned it. */
class Entry {
  constructor(holder, topic, lifetime, mutationSeq) {
    this.holder = holder;
    this.topic = topic;
    this.lifetime = lifetime;
    this.mutationSeq = mutationSeq;
  }

  fence() {
    return { holder: this.holder.toString(), lifetime: this.lifetime, mutation_seq: this.mutationSeq };
  }

  beat() {
    return { ...this.fence(), topic: this.topic.raw };
  }

  withReply(body) {
    return new Entry(this.holder, this.topic, body.lifetime, body.mutation_seq);
  }
}

/** One joined topic: owns the per-view NATS subscriptions and the Glue reader state machine behind it. */
class TopicView {
  constructor(topic, glue) {
    this.topic = topic;
    this.glue = glue;
    this.localViewId = generateRandomId();
    this.subs = [];
    this.entry = null;
  }

  get state() {
    return this.glue.rawState();
  }

  list(chooser) {
    return this.glue.list(chooser);
  }

  onJoin(callback) {
    this.glue.onJoin = callback;
  }

  onLeave(callback) {
    this.glue.onLeave = callback;
  }

  onSync(callback) {
    this.glue.onSync = callback;
  }
}

/**
 * A Phoenix-shaped browser client for the NATS presence service: joins topics over a real
 * `wsconnect` transport, authenticating with connect tokens an app-supplied `fetchGrant`
 * callback mints, and keeps each joined topic's state in sync via `Glue`.
 */
export class PresenceClient {
  /**
   * @param {object} options
   * @param {import("./values.js").PresenceKey} options.key the caller's own presence identity
   * @param {string[]} options.servers websocket server urls
   * @param {(topics: import("./values.js").Topic[]) => Promise<import("./token-cache.js").ConnectGrant>} options.fetchGrant
   * @param {ShardCount} [options.shards]
   * @param {import("./values.js").HolderId} [options.holderId]
   * @param {number} [options.heartbeatMillis]
   */
  constructor(options) {
    this.key = options.key;
    this.servers = options.servers;
    this.shards = options.shards ?? ShardCount.default();
    this.holderId = options.holderId ?? tabHolderId();
    this.heartbeatMillis = options.heartbeatMillis ?? DEFAULT_HEARTBEAT_MS;
    this.webLock = new WebLockHolder(this.holderId);
    this.subjects = new Subjects();
    this.tokenCache = new TokenCache(options.fetchGrant);
    this.connection = null;
    this.connectionId = null;
    this.currentToken = null;
    this.views = new Map();
    this.pendingJoins = new Map();
    this.joinTimer = null;
    this.lastReconnectAt = 0;
    this.authFailureStreak = 0;
    this.statusLoop = null;
    this.heartbeatTimer = null;
    this.closed = false;

    this.tokenCache.onRefreshed((grant) => {
      this.currentToken = grant.token;
    });
  }

  /** @param {string} rawTopic */
  join(rawTopic) {
    const topic = new Topic(rawTopic);
    const existing = this.views.get(topic.raw);
    if (existing != null) {
      return Promise.resolve(existing);
    }
    const pending = this.pendingJoins.get(topic.raw);
    if (pending != null) {
      return pending.promise;
    }
    let resolve;
    let reject;
    const promise = new Promise((res, rej) => {
      resolve = res;
      reject = rej;
    });
    this.pendingJoins.set(topic.raw, { topic, resolve, reject, promise });
    if (this.joinTimer == null) {
      this.joinTimer = setTimeout(() => {
        this.joinTimer = null;
        this._commitJoins().catch((error) => this._failPendingJoins(error));
      }, JOIN_DEBOUNCE_MS);
    }
    return promise;
  }

  /** @param {string} rawTopic */
  async leave(rawTopic) {
    const topic = new Topic(rawTopic);
    const view = this.views.get(topic.raw);
    if (view == null) return;
    if (view.entry != null) {
      await this._release([view.entry]);
      view.entry = null;
    }
    this._teardownView(view);
    this.views.delete(topic.raw);
    await this._reconnectForDesiredTopics();
  }

  _failPendingJoins(error) {
    for (const { reject } of this.pendingJoins.values()) reject(error);
    this.pendingJoins.clear();
  }

  async _commitJoins() {
    const requested = [...this.pendingJoins.values()];
    this.pendingJoins.clear();
    const desired = new Map(this.views);
    for (const { topic } of requested) {
      if (!desired.has(topic.raw)) desired.set(topic.raw, null);
    }
    const grant = await this.tokenCache.forceRefresh([...desired.keys()].map((raw) => new Topic(raw)));
    const accepted = [];
    for (const { topic, resolve, reject } of requested) {
      const denial = denialFor(grant, topic);
      if (denial != null) {
        desired.delete(topic.raw);
        reject(new JoinError(topic.raw, denial.reason, `topic ${topic.raw} was denied: ${denial.reason}`));
        continue;
      }
      accepted.push({ topic, resolve });
    }
    const desiredTopics = [...desired.keys()].map((raw) => new Topic(raw));
    const changed = desiredTopics.length !== this.views.size || accepted.length > 0 || this.connection == null;
    if (!changed) return;
    this.connectionId = grant.connectionId;
    this.currentToken = grant.token;
    await this._reconnect();
    for (const topic of desiredTopics) {
      if (!this.views.has(topic.raw)) {
        const view = this._createView(topic);
        this.views.set(topic.raw, view);
      }
      await this._subscribeView(this.views.get(topic.raw));
    }
    for (const { topic, resolve } of accepted) {
      resolve(this.views.get(topic.raw));
    }
  }

  async _reconnectForDesiredTopics() {
    const desiredTopics = [...this.views.keys()].map((raw) => new Topic(raw));
    const grant = await this.tokenCache.forceRefresh(desiredTopics);
    this.connectionId = grant.connectionId;
    this.currentToken = grant.token;
    await this._reconnect();
    for (const topic of desiredTopics) {
      await this._subscribeView(this.views.get(topic.raw));
    }
  }

  async _reconnect() {
    const wait = MIN_FORCED_RECONNECT_MS - (Date.now() - this.lastReconnectAt);
    if (wait > 0) await sleep(wait);
    if (this.authFailureStreak > 0) {
      await sleep(Math.min(MAX_AUTH_BACKOFF_MS, 1_000 * 2 ** this.authFailureStreak));
    }
    for (const view of this.views.values()) this._unsubscribeView(view);
    this.lastReconnectAt = Date.now();
    try {
      // A fresh connect, never `connection.reconnect()`: nats-core's shared request/reply
      // mux inbox is computed once from `inboxPrefix` and never recomputed on `.reconnect()`,
      // so reusing the old connection after a connectionId change would keep issuing
      // `connection.request()` calls (track/update/untrack/heartbeat/release) against an
      // inbox the new grant does not authorize.
      if (this.connection != null) {
        await this.connection.close();
      }
      this.connection = await wsconnect({
        servers: this.servers,
        authenticator: tokenAuthenticator(() => this.currentToken),
        ignoreAuthErrorAbort: true,
        reconnect: true,
        maxReconnectAttempts: -1,
        waitOnFirstConnect: true,
        inboxPrefix: connectionInboxPrefix(this.key, this.connectionId),
      });
      this._watchStatus();
      this.authFailureStreak = 0;
    } catch (error) {
      this.authFailureStreak += 1;
      throw error;
    }
  }

  _watchStatus() {
    this.statusLoop = (async () => {
      for await (const status of this.connection.status()) {
        if (status.type === "disconnect") {
          this.tokenCache.forceRefresh([...this.views.keys()].map((raw) => new Topic(raw))).catch(() => {});
        }
        if (status.type === "reconnect") {
          for (const view of this.views.values()) view.glue.resync("transport reconnected");
        }
      }
    })();
  }

  _createView(topic) {
    const glue = new Glue({
      onResyncNeeded: (reason) => this._scheduleResync(topic, reason),
      sha256: sha256Hex,
    });
    return new TopicView(topic, glue);
  }

  _scheduleResync(topic, _reason) {
    setTimeout(() => {
      const view = this.views.get(topic.raw);
      if (view == null || view.glue.pending != null) return;
      this._requestSnapshot(view).catch(() => {});
    }, jittered(RESYNC_JITTER_MS));
  }

  _unsubscribeView(view) {
    for (const sub of view.subs) sub.unsubscribe();
    view.subs = [];
  }

  _teardownView(view) {
    this._unsubscribeView(view);
  }

  async _subscribeView(view) {
    const nc = this.connection;
    const key = this.key;
    const connectionId = this.connectionId;
    const inboxPrefix = `${connectionInboxPrefix(key, connectionId)}.${view.localViewId}`;
    const diffSub = nc.subscribe(this.subjects.diffSubject(view.topic));
    const partsSub = nc.subscribe(this.subjects.snapshotReplyWildcard(key, connectionId));
    const repliesSub = nc.subscribe(`${inboxPrefix}.*`);
    const shardToken = this.shards.token(viewShardIndex(view.topic.raw, this.shards));
    const epochSub = nc.subscribe(this.subjects.epochSubject(shardToken));
    view.subs = [diffSub, partsSub, repliesSub, epochSub];
    view.inboxPrefix = inboxPrefix;
    view.shardToken = shardToken;
    this._pump(diffSub, (msg) => this._onDiffMessage(view, msg));
    this._pump(partsSub, (msg) => this._onPartMessage(view, msg));
    this._pump(repliesSub, (msg) => this._onReplyMessage(view, msg));
    this._pump(epochSub, (msg) => this._onEpochMessage(view, msg));
    await nc.flush();
    await this._requestSnapshot(view);
  }

  _pump(sub, handle) {
    (async () => {
      for await (const msg of sub) handle(msg);
    })().catch(() => {});
  }

  /**
   * A snapshot request is a plain `publish`, not a `request`: nats-server never reports "no
   * responders" for it, so a request published while the owning shard is between a lease
   * handoff and its writer resubscribing is silently lost with no error to react to. The
   * watchdog below is the only thing that notices and tries again, bounded by a wall-clock
   * deadline carried across retries (not reset per attempt) so a permanently denied or torn-down
   * view eventually stops retrying instead of polling forever.
   */
  async _requestSnapshot(view, retry = {}) {
    if (view.glue.pending != null) return;
    const requestId = generateRandomId();
    view.glue.beginRequest(requestId);
    const inbox = `${view.inboxPrefix}.${requestId}`;
    const subject = this.subjects.snapshotSubject(view.shardToken, this.key, this.connectionId, view.topic);
    const body = new TextEncoder().encode(JSON.stringify({ request_id: requestId }));
    this.connection.publish(subject, body, { reply: inbox });
    const deadline = retry.deadline ?? Date.now() + SNAPSHOT_RETRY_DEADLINE_MS;
    this._watchSnapshotRequest(view, requestId, deadline, retry.attempt ?? 0);
  }

  _watchSnapshotRequest(view, requestId, deadline, attempt) {
    setTimeout(() => {
      if (this.views.get(view.topic.raw) !== view) return;
      if (view.glue.pending?.requestId !== requestId) return;
      if (Date.now() >= deadline) return;
      view.glue.abandonPending(requestId);
      this._requestSnapshot(view, { deadline, attempt: attempt + 1 }).catch(() => {});
    }, fullJitterBackoff(SNAPSHOT_RETRY_BASE_MS, SNAPSHOT_RETRY_CAP_MS, attempt));
  }

  _onDiffMessage(view, msg) {
    const envelope = frameEnvelope(msg);
    if (envelope == null) return;
    const payload = envelope.kind === "diff" ? JSON.parse(new TextDecoder().decode(msg.data)) : undefined;
    view.glue.receive({ ...envelope, payload });
  }

  _onPartMessage(view, msg) {
    const envelope = frameEnvelope(msg);
    if (envelope == null) return;
    view.glue.receive({ ...envelope, payload: msg.data });
  }

  _onEpochMessage(view, msg) {
    const payload = JSON.parse(new TextDecoder().decode(msg.data));
    view.glue.receive({ kind: "epoch", payload });
  }

  _onReplyMessage(view, msg) {
    const pending = view.glue.pending;
    if (pending == null) return;
    const addressedTo = msg.subject.slice(msg.subject.lastIndexOf(".") + 1);
    if (addressedTo !== pending.requestId) return;
    const code = header(msg, HEADER_CODE);
    if (code !== "ok") return;
    const manifest = JSON.parse(new TextDecoder().decode(msg.data));
    const record = {
      kind: "manifest",
      generation: header(msg, HEADER_GENERATION),
      owner_rev: header(msg, HEADER_OWNER_REV),
      owner_id: header(msg, HEADER_OWNER_ID),
      seq: Number(header(msg, HEADER_SEQ)),
      requestId: manifest.request_id,
      payload: manifest,
    };
    view.glue.receive(record);
  }

  async _command(subject, body) {
    const payload = new TextEncoder().encode(JSON.stringify(body));
    const deadline = Date.now() + COMMAND_RETRY_DEADLINE_MS;
    for (let attempt = 0; ; attempt += 1) {
      const reply = await this.connection.request(subject, payload, { timeout: COMMAND_TIMEOUT_MS });
      const code = header(reply, HEADER_CODE);
      const replyBody = reply.data.length === 0 ? {} : JSON.parse(new TextDecoder().decode(reply.data));
      if (code === "ok") {
        return replyBody;
      }
      const retryable = RETRYABLE_CODES.includes(code);
      if (!retryable || attempt >= COMMAND_RETRY_ATTEMPTS || Date.now() >= deadline) {
        throw new PresenceError(code ?? ErrorCode.Unavailable, `${subject} replied ${code}`, { topic: subject });
      }
      await sleep(fullJitterBackoff(COMMAND_RETRY_BASE_MS, COMMAND_RETRY_CAP_MS, attempt));
    }
  }

  /** @param {TopicView} view @param {Record<string, unknown>} meta */
  async track(view, meta) {
    assertSafeMeta(meta);
    const body = await this._command(this.subjects.trackSubject(this.key, view.topic), {
      holder: this.holderId.toString(),
      meta,
    });
    view.entry = new Entry(this.holderId, view.topic, body.lifetime, body.mutation_seq);
    this._ensureHeartbeat();
    return view.entry;
  }

  /** @param {TopicView} view @param {Record<string, unknown>} meta */
  async update(view, meta) {
    assertSafeMeta(meta);
    if (view.entry == null) {
      throw new PresenceError(ErrorCode.Gone, `topic ${view.topic.raw} is not tracked`);
    }
    const body = await this._command(this.subjects.updateSubject(this.key, view.topic), {
      ...view.entry.fence(),
      meta,
    });
    view.entry = view.entry.withReply(body);
    return view.entry;
  }

  /** @param {TopicView} view */
  async untrack(view) {
    if (view.entry == null) return;
    await this._command(this.subjects.untrackSubject(this.key, view.topic), view.entry.fence());
    view.entry = null;
  }

  async _release(entries) {
    if (entries.length === 0) return;
    const targets = entries.map((entry) => ({ topic: entry.topic.raw, lifetime: entry.lifetime }));
    await this._command(this.subjects.releaseSubject(this.key), {
      holder: this.holderId.toString(),
      targets,
    });
  }

  _ensureHeartbeat() {
    if (this.heartbeatTimer != null) return;
    this.heartbeatTimer = setInterval(() => {
      const entries = [...this.views.values()].map((view) => view.entry).filter((entry) => entry != null);
      if (entries.length === 0) return;
      this._command(this.subjects.heartbeatSubject(this.key), {
        entries: entries.map((entry) => entry.beat()),
      }).catch(() => {});
    }, this.heartbeatMillis);
  }

  async close() {
    if (this.closed) return;
    this.closed = true;
    if (this.joinTimer != null) clearTimeout(this.joinTimer);
    if (this.heartbeatTimer != null) clearInterval(this.heartbeatTimer);
    const entries = [...this.views.values()].map((view) => view.entry).filter((entry) => entry != null);
    try {
      if (this.connection != null) await this._release(entries);
    } catch {
      // best effort: the connection may already be gone
    }
    for (const view of this.views.values()) this._teardownView(view);
    this.views.clear();
    this.tokenCache.close();
    if (this.connection != null) await this.connection.close();
  }
}
