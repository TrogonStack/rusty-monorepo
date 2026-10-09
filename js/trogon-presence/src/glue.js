import Presence from "../vendor/presence.v1.8.1.mjs";
import { StateAdapter } from "./state-adapter.js";

function compareEpoch(seen, next) {
  if (seen.generation !== next.generation) return "cross";
  const seenAcquired = BigInt(seen.acquired);
  const nextAcquired = BigInt(next.acquired);
  if (seenAcquired < nextAcquired) return "older";
  if (seenAcquired > nextAcquired) return "newer";
  return seen.owner === next.owner ? "same" : "conflict";
}

function follow(seen, next, prev) {
  const order = compareEpoch(seen.epoch, next.epoch);
  if (order === "older") return "rebase";
  if (order === "newer") return "stale";
  if (order === "conflict" || order === "cross") return "rebase";
  const seenSeq = seen.seq;
  const offeredSeq = next.seq;
  if (offeredSeq === seenSeq && prev === seenSeq) return "repeat";
  if (offeredSeq <= seenSeq) return "stale";
  if (prev === seenSeq) return "next";
  return "gap";
}

function concatBytes(chunks) {
  const total = chunks.reduce((sum, chunk) => sum + chunk.length, 0);
  const out = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    out.set(chunk, offset);
    offset += chunk.length;
  }
  return out;
}

/**
 * A browser-side port of `trogon_presence_service::reader::Machine`: applies manifest/snapshot-part/snapshot-end/
 * diff/keepalive/epoch records against an installed service cursor, buffering diffs while a snapshot is in flight
 * and asking its owner to send a fresh snapshot request (`onResyncNeeded`) whenever the chain breaks. The owner
 * drives it from live NATS messages, calling `beginRequest` right after publishing a snapshot request.
 */
export class Glue {
  /** @param {{ onJoin?: Function, onLeave?: Function, onSync?: Function, onResyncNeeded: (reason: string) => void, sha256: (bytes: Uint8Array) => Promise<string> }} options */
  constructor(options) {
    this.adapter = new StateAdapter();
    this.onJoin = options.onJoin;
    this.onLeave = options.onLeave;
    this.onSync = options.onSync || (() => {});
    this.onResyncNeeded = options.onResyncNeeded;
    this.sha256 = options.sha256;
    this.installed = null;
    this.pending = null;
    this.resyncs = 0;
  }

  get state() {
    return this.adapter.state;
  }

  rawState() {
    return this.adapter.rawState();
  }

  /** @param {(key: string, presence: { metas: unknown[] }) => unknown} [chooser] mirrors phoenix.js Presence#list */
  list(chooser) {
    return Presence.list(this.rawState(), chooser);
  }

  /** Call right after publishing a snapshot request, with the same request id used on the wire. */
  beginRequest(requestId) {
    this.pending = { requestId, assembly: null, stash: [], diffBuffer: [] };
  }

  async receive(record) {
    switch (record.kind) {
      case "epoch":
        return this.onEpoch(record);
      case "manifest":
        return this.onManifest(record);
      case "snapshot-part":
        return this.onPart(record);
      case "snapshot-end":
        return this.onEnd(record);
      case "diff":
      case "keepalive":
        return this.onDiff(record);
      default:
        throw new Error(`unknown record kind ${record.kind}`);
    }
  }

  resync(reason) {
    if (this.pending != null) return;
    this.resyncs += 1;
    this.onResyncNeeded(reason);
  }

  onEpoch(record) {
    const hinted = { generation: record.payload.generation, acquired: record.payload.owner_epoch.acquired, owner: record.payload.owner_epoch.owner };
    if (this.installed == null) return this.resync("epoch hint arrived before any snapshot was installed");
    const order = compareEpoch(this.installed.cursor.epoch, hinted);
    if (order === "same" || order === "newer") return;
    this.resync(`epoch hint supersedes the installed cursor (${order})`);
  }

  onManifest(record) {
    if (this.pending == null || record.requestId !== this.pending.requestId || this.pending.assembly != null) return;
    const manifest = record.payload;
    this.pending.assembly = {
      epoch: { generation: record.generation, acquired: record.owner_rev, owner: record.owner_id },
      seq: record.seq,
      parts: manifest.parts,
      totalBytes: manifest.total_bytes,
      sha256: manifest.sha256,
      chunks: new Map(),
      ended: false,
    };
    const stash = this.pending.stash;
    this.pending.stash = [];
    return this.drainStash(stash);
  }

  async drainStash(stash) {
    for (const frame of stash) {
      if (await this.feed(frame)) return;
    }
  }

  onPart(record) {
    if (this.pending == null || record.requestId !== this.pending.requestId) return;
    if (this.pending.assembly != null) return this.feed(record);
    this.pending.stash.push(record);
  }

  onEnd(record) {
    if (this.pending == null || record.requestId !== this.pending.requestId) return;
    if (this.pending.assembly != null) return this.feed(record);
    this.pending.stash.push(record);
  }

  /** @returns {Promise<boolean>} true once the pending request has resolved (installed or abandoned via resync) */
  async feed(record) {
    const assembly = this.pending?.assembly;
    if (assembly == null) return true;
    if (record.kind === "snapshot-part") {
      assembly.chunks.set(record.part, record.payload);
    } else if (record.kind === "snapshot-end") {
      if (record.parts !== assembly.parts) {
        this.resync(`snapshot end counts ${record.parts} parts, manifest promised ${assembly.parts}`);
        return true;
      }
      assembly.ended = true;
    }
    return this.maybeComplete();
  }

  async maybeComplete() {
    const pending = this.pending;
    const assembly = pending?.assembly;
    if (assembly == null || !assembly.ended || assembly.chunks.size !== assembly.parts) return false;
    const ordered = Array.from({ length: assembly.parts }, (_, index) => assembly.chunks.get(index + 1));
    if (ordered.some((chunk) => chunk === undefined)) return false;
    const bytes = concatBytes(ordered);
    if (bytes.length !== assembly.totalBytes) {
      this.resync(`snapshot carries ${bytes.length} bytes, manifest promised ${assembly.totalBytes}`);
      return true;
    }
    const digest = await this.sha256(bytes);
    if (digest !== assembly.sha256) {
      this.resync(`snapshot digest ${digest} does not match manifest ${assembly.sha256}`);
      return true;
    }
    const state = JSON.parse(new TextDecoder("utf-8").decode(bytes));
    this.install({ epoch: assembly.epoch, seq: assembly.seq }, state, pending.diffBuffer);
    return true;
  }

  install(cursor, state, buffered) {
    this.pending = null;
    this.installed = { cursor };
    this.adapter.syncState(state, this.onJoin, this.onLeave);
    this.onSync();
    for (const buffered1 of buffered) {
      if (this.pending != null) break;
      this.applyFrame(buffered1.record, buffered1.frameCursor, buffered1.prev);
    }
  }

  onDiff(record) {
    const frameCursor = { epoch: { generation: record.generation, acquired: record.owner_rev, owner: record.owner_id }, seq: record.seq };
    if (this.pending != null) {
      this.pending.diffBuffer.push({ record, frameCursor, prev: record.prev });
      return;
    }
    this.applyFrame(record, frameCursor, record.prev);
  }

  applyFrame(record, frameCursor, prev) {
    if (this.installed == null) return this.resync(`${record.kind} frame arrived before any snapshot was installed`);
    const step = follow(this.installed.cursor, frameCursor, prev);
    if (step === "repeat" || step === "stale") return;
    if (step === "gap" || step === "rebase") return this.resync(`cursor ${step} on a ${record.kind} frame`);
    this.installed.cursor = frameCursor;
    if (record.kind === "diff") {
      this.adapter.syncDiff(record.payload, this.onJoin, this.onLeave);
      this.onSync();
    }
  }
}
