import { PresenceClient } from "../../index.js";
import { HolderId, PresenceKey } from "../../src/values.js";
import { generateRandomId } from "../../src/ids.js";
import { createFetchGrant } from "./grant.js";
import { wsUrl } from "./rpc.js";

export const DEFAULT_SESSION_SECS = 3600;
export const TENANT_A = "alpha";
export const TENANT_B = "beta";

export function freshHolderId() {
  return new HolderId(generateRandomId());
}

/**
 * Builds a real `PresenceClient` wired to the Rust RPC sidecar's `enroll`/`refresh`
 * commands for one (tenant, sub, sid) identity, connecting over the fixture's real
 * websocket listener.
 *
 * @param {import("./rpc.js").RpcClient} rpc
 * @param {{ tenant?: string, sub: string, sid?: string, sessionSecs?: number }} identity
 */
export function createClient(rpc, identity) {
  const resolved = {
    tenant: identity.tenant ?? TENANT_A,
    sub: identity.sub,
    sid: identity.sid ?? "s1",
    sessionSecs: identity.sessionSecs ?? DEFAULT_SESSION_SECS,
  };
  const client = new PresenceClient({
    key: new PresenceKey(resolved.sub),
    servers: [wsUrl()],
    holderId: freshHolderId(),
    fetchGrant: createFetchGrant(rpc, resolved),
  });
  client.identity = resolved;
  return client;
}

/** Polls `predicate` until it returns truthy, or rejects once `withinMillis` elapses. */
export async function waitFor(predicate, withinMillis = 5_000, description = "condition") {
  const deadline = Date.now() + withinMillis;
  while (Date.now() < deadline) {
    const outcome = await predicate();
    if (outcome) return outcome;
    await sleep(25);
  }
  throw new Error(`timed out waiting for ${description}`);
}

export function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Resolves the next time `callback` is invoked, carrying its arguments, or rejects once
 * `withinMillis` elapses: a callback that never fires must fail the test clearly, not hang it. */
export function nextCall(install, withinMillis = 15_000) {
  let resolve;
  let reject;
  const promise = new Promise((res, rej) => {
    resolve = res;
    reject = rej;
  });
  const timer = setTimeout(() => reject(new Error("timed out waiting for the callback to fire")), withinMillis);
  install((...args) => {
    clearTimeout(timer);
    resolve(args);
  });
  return promise;
}

export async function closeAll(...clients) {
  await Promise.all(clients.map((client) => client.close().catch(() => {})));
}
