import { Milliseconds } from "./values.js";

const REFRESH_LEEWAY = new Milliseconds(5_000);
const MIN_REFRESH_DELAY = new Milliseconds(250);
// How long to wait before re-checking whether a redial is still in flight, once a proactive
// refresh has deferred itself for that reason.
const REDIAL_RECHECK_MS = new Milliseconds(500);

function base64UrlDecode(segment) {
  const padded = segment.replace(/-/g, "+").replace(/_/g, "/");
  const pad = padded.length % 4 === 0 ? "" : "=".repeat(4 - (padded.length % 4));
  const binary = atob(padded + pad);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

export class TokenError extends Error {}

/**
 * Reads the exp claim (seconds since epoch) out of a compact JWS, without verifying
 * its signature. Only used client-side to schedule a proactive refresh; the server
 * performs the real validation.
 * @param {string} compactJws
 */
function unverifiedExpiryMillis(compactJws) {
  const parts = compactJws.split(".");
  if (parts.length !== 3) {
    throw new TokenError("connect token is not a compact JWS");
  }
  let payload;
  try {
    payload = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(base64UrlDecode(parts[1])));
  } catch (error) {
    throw new TokenError("connect token payload is not valid JSON", { cause: error });
  }
  if (typeof payload.exp !== "number") {
    throw new TokenError("connect token payload is missing exp");
  }
  return payload.exp * 1000;
}

/**
 * @typedef {object} TopicDenial
 * @property {import("./values.js").Topic} topic
 * @property {string} reason
 */

/**
 * @typedef {object} ConnectGrant
 * @property {string} token compact JWS connect token, authorizing only the topics not listed in deniedTopics
 * @property {import("./values.js").ConnectionId} connectionId
 * @property {TopicDenial[]} deniedTopics topics the app backend refused to grant, each surfaced as a join error instead of a reconnect
 */

/** @param {ConnectGrant} grant @param {import("./values.js").Topic} topic */
export function denialFor(grant, topic) {
  return grant.deniedTopics.find((denial) => denial.topic.toString() === topic.toString()) ?? null;
}

/**
 * Caches the current connect token for a topic set and refreshes it asynchronously
 * before it expires or whenever the transport disconnects. A topic-set change always
 * forces a fresh fetch, since the contract's enroll command (not refresh_session)
 * is the only way to change authorized topics.
 */
export class TokenCache {
  /**
   * @param {(topics: import("./values.js").Topic[]) => Promise<ConnectGrant>} fetchGrant
   * @param {object} [options]
   * @param {() => boolean} [options.isRedialing] reports whether the transport is mid an
   *   automatic redial. The proactive refresh timer defers while this is true, since the
   *   service bumps the connection's AuthVersion on every refresh and would fence out
   *   whatever token the redial is presenting mid-handshake. A disconnect-triggered `get()`
   *   is exempt: it is the one refresh an actually stale cache needs before a redial can
   *   possibly succeed at all.
   */
  constructor(fetchGrant, options = {}) {
    this._fetchGrant = fetchGrant;
    this._isRedialing = options.isRedialing ?? (() => false);
    this._topicKey = null;
    this._grant = null;
    this._expiryMillis = 0;
    this._inflight = null;
    this._refreshTimer = null;
    this._onRefreshed = null;
  }

  /** @param {(grant: ConnectGrant) => void} callback */
  onRefreshed(callback) {
    this._onRefreshed = callback;
  }

  /** @param {import("./values.js").Topic[]} topics */
  async get(topics) {
    const key = topics.map((topic) => topic.toString()).sort().join("\u0000");
    if (key !== this._topicKey || this._grant === null) {
      this._topicKey = key;
      return this._refresh(topics);
    }
    if (Date.now() >= this._expiryMillis - REFRESH_LEEWAY.value) {
      return this._refresh(topics);
    }
    return this._grant;
  }

  /** Forces a fresh grant, as required after a disconnect. */
  async forceRefresh(topics) {
    this._topicKey = topics.map((topic) => topic.toString()).sort().join("\u0000");
    return this._refresh(topics);
  }

  async _refresh(topics) {
    if (this._inflight !== null) {
      return this._inflight;
    }
    this._inflight = this._fetchGrant(topics)
      .then((grant) => {
        this._grant = grant;
        this._expiryMillis = unverifiedExpiryMillis(grant.token);
        this._scheduleProactiveRefresh(topics);
        if (this._onRefreshed !== null) {
          this._onRefreshed(grant);
        }
        return grant;
      })
      .finally(() => {
        this._inflight = null;
      });
    return this._inflight;
  }

  _scheduleProactiveRefresh(topics) {
    if (this._refreshTimer !== null) {
      clearTimeout(this._refreshTimer);
    }
    const now = Date.now();
    const aheadOfLeeway = this._expiryMillis - REFRESH_LEEWAY.value - now;
    // A session whose remaining lifetime is at or below REFRESH_LEEWAY has no meaningful
    // "leeway before expiry" point: that arithmetic stays near zero forever, which would
    // refire at MIN_REFRESH_DELAY in a tight loop, bumping the connection's AuthVersion fast
    // enough to fence out tokens still in flight to the server during a reconnect. Back off
    // to half the remaining lifetime instead, so the rate decays as expiry approaches rather
    // than spinning at the floor.
    const delay =
      aheadOfLeeway > MIN_REFRESH_DELAY.value
        ? aheadOfLeeway
        : Math.max(MIN_REFRESH_DELAY.value, (this._expiryMillis - now) / 2);
    this._refreshTimer = setTimeout(() => this._fireProactiveRefresh(topics), delay);
  }

  _fireProactiveRefresh(topics) {
    if (this._isRedialing()) {
      this._refreshTimer = setTimeout(() => this._fireProactiveRefresh(topics), REDIAL_RECHECK_MS.value);
      return;
    }
    this._refresh(topics).catch(() => {});
  }

  close() {
    if (this._refreshTimer !== null) {
      clearTimeout(this._refreshTimer);
      this._refreshTimer = null;
    }
  }
}
