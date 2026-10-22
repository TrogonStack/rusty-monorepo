import { createHash } from "node:crypto";
import { readdir, readFile, stat } from "node:fs/promises";
import path from "node:path";
import { isDeepStrictEqual } from "node:util";

const PRESENCE_SOURCE = new URL("./vendor/presence.v1.8.1.mjs", import.meta.url);
const PRESENCE_SHA256 = "c6b2b66730d8282bef563dc465fef081e9f4de40334507b99fa7e46d3bb8fea6";
const MAX_RAW_KEY_BYTES = 256;

const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

const vendored = await readFile(PRESENCE_SOURCE);
if (sha256(vendored) !== PRESENCE_SHA256) {
  console.error(`vendored Presence helper digest ${sha256(vendored)} != pinned ${PRESENCE_SHA256}`);
  process.exit(1);
}
const { default: Presence } = await import(PRESENCE_SOURCE.href);

const clone = (value) => structuredClone(value);

class ConformanceError extends Error {}

const rawKeyBytes = (key) => {
  if (typeof key !== "string" || !key.isWellFormed()) throw new ConformanceError(`presence key ${key} is not valid UTF-8`);
  const bytes = Buffer.from(key, "utf8");
  if (bytes.length === 0 || bytes.length > MAX_RAW_KEY_BYTES) {
    throw new ConformanceError(`presence key ${JSON.stringify(key)} is outside the raw byte bounds`);
  }
  return bytes;
};

const encodeKey = (key) =>
  "p-" + [...rawKeyBytes(key)].map((byte) => `=${byte.toString(16).toUpperCase().padStart(2, "0")}`).join("");

const decodeKey = (key) => {
  if (!/^p-(?:=[0-9A-F]{2})+$/.test(key)) throw new ConformanceError(`helper key ${key} is not canonical`);
  const bytes = Uint8Array.from(key.slice(2).split("=").slice(1), (hex) => Number.parseInt(hex, 16));
  const decoded = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes);
  if (encodeKey(decoded) !== key) throw new ConformanceError(`helper key ${key} does not round-trip`);
  return decoded;
};

const encodeMap = (entries) => {
  const result = Object.create(null);
  for (const [key, value] of entries) result[encodeKey(key)] = clone(value);
  return result;
};

const decodeMap = (map) => new Map(Object.getOwnPropertyNames(map).map((key) => [decodeKey(key), map[key]]));

const decodedCallback = (callback) => callback && ((key, current, changed) => callback(decodeKey(key), current, changed));

const ownEntries = (object) => Object.getOwnPropertyNames(object).map((key) => [key, object[key]]);

class KeyAdapter {
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
    if (!Object.hasOwn(change, "joins") || !Object.hasOwn(change, "leaves")) {
      throw new ConformanceError("diff without both joins and leaves");
    }
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

  rawState() {
    const result = Object.create(null);
    for (const [key, value] of this.state) result[key] = value;
    return result;
  }
}

const epochOf = (record) => ({
  generation: record.generation,
  ownerRev: Number(record.owner_rev),
  ownerId: record.owner_id,
});

const epochKey = (epoch) => (epoch == null ? "none" : `${epoch.generation}/${epoch.ownerRev}/${epoch.ownerId}`);

const sameEpoch = (left, right) => epochKey(left) === epochKey(right);

const olderEpoch = (candidate, current) =>
  current != null && candidate.generation === current.generation && candidate.ownerRev < current.ownerRev;

class Glue {
  constructor() {
    this.adapter = new KeyAdapter();
    this.epoch = null;
    this.lastSeq = null;
    this.syncing = true;
    this.buffer = [];
    this.assembly = null;
    this.resyncs = [];
    this.applied = { diff: 0, keepalive: 0, snapshot: 0, parts: 0 };
  }

  get state() {
    return this.adapter.state;
  }

  receive(record) {
    switch (record.kind) {
      case "epoch":
        return this.announce(record);
      case "manifest":
        return this.manifest(record);
      case "snapshot-part":
        return this.part(record);
      case "snapshot-end":
        return this.end(record);
      case "diff":
      case "keepalive":
        return this.frame(record);
      default:
        throw new ConformanceError(`unknown record kind ${record.kind}`);
    }
  }

  resync(reason) {
    if (this.syncing) return;
    this.resyncs.push(reason);
    this.syncing = true;
    this.buffer = [];
  }

  announce(record) {
    const epoch = epochOf(record);
    if (!sameEpoch(epoch, this.epoch)) this.resync(`epoch ${epochKey(this.epoch)} -> ${epochKey(epoch)}`);
  }

  manifest(record) {
    if (!this.syncing) return;
    const manifest = record.payload;
    const epoch = epochOf(record);
    if (
      Number(manifest.seq) !== record.seq ||
      manifest.generation !== epoch.generation ||
      Number(manifest.owner_epoch.acquired) !== epoch.ownerRev ||
      manifest.owner_epoch.owner !== epoch.ownerId
    ) {
      throw new ConformanceError(`manifest ${manifest.snapshot_id} disagrees with its headers`);
    }
    this.assembly = {
      snapshotId: manifest.snapshot_id,
      requestId: manifest.request_id,
      epoch,
      seq: record.seq,
      parts: manifest.parts,
      totalBytes: manifest.total_bytes,
      sha256: manifest.sha256,
      chunks: new Map(),
      ended: false,
    };
  }

  owned(record) {
    const assembly = this.assembly;
    if (assembly == null || record.snapshot_id !== assembly.snapshotId) return null;
    if (
      record.request_id !== assembly.requestId ||
      record.seq !== assembly.seq ||
      !sameEpoch(epochOf(record), assembly.epoch)
    ) {
      throw new ConformanceError(`snapshot ${assembly.snapshotId} mixes identities or cursors`);
    }
    return assembly;
  }

  part(record) {
    const assembly = this.owned(record);
    if (assembly == null) return;
    if (!Number.isInteger(record.part) || record.part < 1 || record.part > assembly.parts) {
      throw new ConformanceError(`snapshot part ${record.part} is outside 1..${assembly.parts}`);
    }
    const bytes = Buffer.from(record.payload);
    const seen = assembly.chunks.get(record.part);
    if (seen != null && !seen.equals(bytes)) {
      throw new ConformanceError(`snapshot part ${record.part} conflicts with an earlier copy`);
    }
    assembly.chunks.set(record.part, bytes);
    this.complete(assembly);
  }

  end(record) {
    const assembly = this.owned(record);
    if (assembly == null) return;
    if (record.parts !== assembly.parts) {
      throw new ConformanceError(`snapshot end counts ${record.parts} parts, manifest ${assembly.parts}`);
    }
    assembly.ended = true;
    this.complete(assembly);
  }

  complete(assembly) {
    if (!assembly.ended || assembly.chunks.size !== assembly.parts) return;
    const ordered = Array.from({ length: assembly.parts }, (_, index) => assembly.chunks.get(index + 1));
    const bytes = Buffer.concat(ordered);
    if (bytes.length !== assembly.totalBytes) {
      throw new ConformanceError(`snapshot carries ${bytes.length} bytes, manifest ${assembly.totalBytes}`);
    }
    if (sha256(bytes) !== assembly.sha256) {
      throw new ConformanceError(`snapshot digest ${sha256(bytes)} != manifest ${assembly.sha256}`);
    }
    this.assembly = null;
    this.applied.snapshot += 1;
    this.applied.parts += assembly.parts;
    this.snapshot(JSON.parse(bytes.toString("utf8")), assembly.epoch, assembly.seq);
  }

  frame(frame) {
    const epoch = epochOf(frame);
    if (!this.syncing && !sameEpoch(epoch, this.epoch)) {
      if (olderEpoch(epoch, this.epoch)) return;
      this.resync(`epoch ${epochKey(this.epoch)} -> ${epochKey(epoch)} on a ${frame.kind} frame`);
    }
    if (this.syncing) {
      this.buffer.push(frame);
      return;
    }
    if (frame.kind === "keepalive") {
      if (frame.seq !== this.lastSeq || frame.prev !== this.lastSeq) {
        return this.resync(`keepalive ${frame.prev}->${frame.seq} != last ${this.lastSeq}`);
      }
      this.applied.keepalive += 1;
      return;
    }
    if (frame.seq <= this.lastSeq) return;
    if (frame.prev !== this.lastSeq) {
      this.resync(`diff prev ${frame.prev} != last ${this.lastSeq}`);
      this.buffer.push(frame);
      return;
    }
    this.diff(frame);
  }

  diff(frame) {
    const { joins, leaves } = frame.payload;
    for (const [key, { metas }] of ownEntries(leaves)) {
      const joined = new Set(((Object.hasOwn(joins, key) && joins[key].metas) || []).map((meta) => meta.phx_ref));
      for (const meta of metas) {
        if (joined.has(meta.phx_ref)) {
          throw new ConformanceError(`diff seq ${frame.seq} carries ref ${meta.phx_ref} of ${key} on both sides`);
        }
      }
    }
    this.adapter.syncDiff(frame.payload);
    for (const [key] of ownEntries(joins)) {
      if (!this.state.has(key)) throw new ConformanceError(`diff seq ${frame.seq} emptied joined key ${key}`);
    }
    this.lastSeq = frame.seq;
    this.applied.diff += 1;
  }

  snapshot(state, epoch, seq) {
    this.adapter.syncState(state);
    this.epoch = epoch;
    this.lastSeq = seq;
    this.syncing = false;
    const buffered = this.buffer;
    this.buffer = [];
    for (const frame of buffered) {
      if (this.syncing) {
        this.buffer.push(frame);
        continue;
      }
      const epoch = epochOf(frame);
      if (sameEpoch(epoch, this.epoch) && frame.seq <= this.lastSeq) continue;
      if (olderEpoch(epoch, this.epoch)) continue;
      this.frame(frame);
    }
  }
}

const normalize = (entries) =>
  Object.fromEntries(
    [...entries]
      .sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0))
      .map(([key, { metas }]) => [
        key,
        { metas: [...metas].sort((a, b) => String(a.phx_ref).localeCompare(String(b.phx_ref))) },
      ]),
  );

const describe = (actual, expected) => {
  const lines = [];
  for (const key of new Set([...Object.keys(actual), ...Object.keys(expected)])) {
    if (!(key in expected)) lines.push(`  + ${key}: ${JSON.stringify(actual[key])}`);
    else if (!(key in actual)) lines.push(`  - ${key}: ${JSON.stringify(expected[key])}`);
    else if (!isDeepStrictEqual(actual[key], expected[key])) {
      lines.push(`  ~ ${key}`, `      client:   ${JSON.stringify(actual[key])}`, `      expected: ${JSON.stringify(expected[key])}`);
    }
  }
  return lines.join("\n");
};

async function replay(file) {
  const records = (await readFile(file, "utf8"))
    .split("\n")
    .filter((line) => line.trim() !== "")
    .map((line, index) => {
      try {
        return JSON.parse(line);
      } catch (err) {
        throw new ConformanceError(`line ${index + 1}: ${err.message}`);
      }
    });
  const header = records.find((record) => record.kind === "scenario");
  const expected = records.find((record) => record.kind === "expected");
  if (!header || !expected) throw new ConformanceError("missing scenario or expected record");

  const glue = new Glue();
  for (const record of records) {
    if (record.kind === "scenario" || record.kind === "expected") continue;
    glue.receive(record);
  }
  let fallback = false;
  if (glue.syncing) {
    fallback = true;
    glue.snapshot(expected.payload, epochOf(expected), expected.seq);
  }

  const problems = [];
  const actual = normalize(glue.state);
  const wanted = normalize(ownEntries(expected.payload));
  if (!isDeepStrictEqual(actual, wanted)) problems.push(`final state differs from the final snapshot:\n${describe(actual, wanted)}`);
  if (!sameEpoch(glue.epoch, epochOf(expected))) {
    problems.push(`epoch ${epochKey(glue.epoch)} != expected ${epochKey(epochOf(expected))}`);
  }
  if (glue.lastSeq !== expected.seq) problems.push(`seq ${glue.lastSeq} != expected ${expected.seq}`);
  if (glue.resyncs.length !== header.resyncs) {
    problems.push(`resyncs ${glue.resyncs.length} != expected ${header.resyncs}: ${JSON.stringify(glue.resyncs)}`);
  }
  if (Object.getPrototypeOf(glue.adapter.rawState()) !== null) problems.push("public state is not a null-prototype map");
  return { name: header.name, problems, glue, fallback, keys: glue.state.size };
}

async function main() {
  const target = path.resolve(process.argv[2] ?? path.join(import.meta.dirname, "golden"));
  const files = (await stat(target)).isDirectory()
    ? (await readdir(target)).filter((name) => name.endsWith(".jsonl")).sort().map((name) => path.join(target, name))
    : [target];
  if (files.length === 0) {
    console.error(`no golden files under ${target}, run mise run presence:conformance:record first`);
    process.exit(1);
  }
  let failed = 0;
  for (const file of files) {
    let outcome;
    try {
      outcome = await replay(file);
    } catch (err) {
      if (!(err instanceof ConformanceError)) throw err;
      outcome = { name: path.basename(file), problems: [err.message] };
    }
    if (outcome.problems.length > 0) {
      failed += 1;
      console.log(`FAIL ${outcome.name}\n${outcome.problems.map((problem) => `  ${problem}`).join("\n")}`);
      continue;
    }
    const { applied, resyncs } = outcome.glue;
    const via = outcome.fallback ? ", resolved by final list" : "";
    console.log(
      `ok   ${outcome.name}: ${outcome.keys} keys, ${applied.diff} diffs, ${applied.snapshot} snapshots ` +
        `(${applied.parts} parts), ${applied.keepalive} keepalive, ${resyncs.length} resyncs${via}` +
        (resyncs.length > 0 ? ` (${resyncs.join("; ")})` : ""),
    );
  }
  console.log(`${files.length - failed}/${files.length} scenarios passed`);
  process.exit(failed === 0 ? 0 : 1);
}

await main();
