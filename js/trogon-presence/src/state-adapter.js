import Presence from "../vendor/presence.v1.8.1.mjs";
import { toSafeKey, fromSafeKey } from "./key-adapter.js";

const encodeMap = (entries) => {
  const result = Object.create(null);
  for (const [key, value] of entries) result[toSafeKey(key)] = structuredClone(value);
  return result;
};

const decodeMap = (map) => new Map(Object.getOwnPropertyNames(map).map((key) => [fromSafeKey(key), map[key]]));

const ownEntries = (object) => Object.getOwnPropertyNames(object).map((key) => [key, object[key]]);

const decodedCallback = (callback) => callback && ((key, current, changed) => callback(fromSafeKey(key), current, changed));

/** Wraps the vendored Phoenix Presence.syncState/syncDiff so raw presence keys survive round-trip through a null-prototype, prototype-pollution-safe state map. */
export class StateAdapter {
  state = new Map();

  syncState(snapshot, onJoin, onLeave) {
    this.state = decodeMap(
      Presence.syncState(
        encodeMap(this.state),
        encodeMap(ownEntries(snapshot)),
        decodedCallback(onJoin),
        decodedCallback(onLeave),
      ),
    );
    return this.state;
  }

  syncDiff(change, onJoin, onLeave) {
    this.state = decodeMap(
      Presence.syncDiff(
        encodeMap(this.state),
        { joins: encodeMap(ownEntries(change.joins)), leaves: encodeMap(ownEntries(change.leaves)) },
        decodedCallback(onJoin),
        decodedCallback(onLeave),
      ),
    );
    return this.state;
  }

  /** @returns {Record<string, { metas: unknown[] }>} a plain object keyed by raw presence keys, safe to hand to the app */
  rawState() {
    const result = Object.create(null);
    for (const [key, value] of this.state) result[key] = value;
    return result;
  }
}
