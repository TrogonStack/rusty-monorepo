import { HolderId } from "./values.js";

const SESSION_STORAGE_KEY = "trogon-presence.holder-id";
const LOCK_NAME_PREFIX = "trogon-presence.holder.";
const RANDOM_ID_BYTES = 16;

function base64UrlEncode(bytes) {
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function randomHolderToken() {
  const bytes = new Uint8Array(RANDOM_ID_BYTES);
  crypto.getRandomValues(bytes);
  return base64UrlEncode(bytes);
}

/**
 * Resolves the caller's HolderId for this tab, minting and persisting one in
 * sessionStorage on first use so it survives reloads within the tab but never
 * leaks to other tabs.
 * @param {Storage} [storage]
 */
export function tabHolderId(storage = sessionStorage) {
  const existing = storage.getItem(SESSION_STORAGE_KEY);
  if (existing !== null) {
    return new HolderId(existing);
  }
  const minted = randomHolderToken();
  storage.setItem(SESSION_STORAGE_KEY, minted);
  return new HolderId(minted);
}

export class WebLockHolderError extends Error {}

/**
 * Serializes operations for one HolderId across tabs using the Web Locks API, so
 * a duplicated tab sharing the same sessionStorage-derived HolderId cannot run two
 * concurrent sessions for it.
 */
export class WebLockHolder {
  /** @param {HolderId} holderId */
  constructor(holderId) {
    this._lockName = LOCK_NAME_PREFIX + holderId.toString();
  }

  /**
   * Runs `fn` only once this holder's lock is acquired; abandons the attempt if
   * `signal` is aborted before acquisition.
   * @template T
   * @param {(signal: AbortSignal) => Promise<T>} fn
   * @param {{ signal?: AbortSignal }} [options]
   * @returns {Promise<T>}
   */
  async withLock(fn, options = {}) {
    if (typeof navigator === "undefined" || !navigator.locks) {
      throw new WebLockHolderError("the Web Locks API is unavailable in this context");
    }
    return navigator.locks.request(this._lockName, { mode: "exclusive", signal: options.signal }, async (lock) => {
      if (lock === null) {
        throw new WebLockHolderError(`lock ${this._lockName} was not granted`);
      }
      const controller = new AbortController();
      try {
        return await fn(controller.signal);
      } finally {
        controller.abort();
      }
    });
  }
}
