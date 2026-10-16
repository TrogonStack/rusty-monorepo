export const HEADER_GENERATION = "Presence-Generation";
export const HEADER_OWNER_REV = "Presence-Owner-Rev";
export const HEADER_OWNER_ID = "Presence-Owner-Id";
export const HEADER_SEQ = "Presence-Seq";
export const HEADER_PREV = "Presence-Prev";
export const HEADER_SHARD = "Presence-Shard";
export const HEADER_TOPIC = "Presence-Topic";
export const HEADER_KIND = "Presence-Kind";
export const HEADER_SNAPSHOT_ID = "Presence-Snapshot-Id";
export const HEADER_REQUEST_ID = "Presence-Request-Id";
export const HEADER_PART = "Presence-Part";
export const HEADER_PARTS = "Presence-Parts";
export const HEADER_CODE = "Presence-Code";

export const FrameKind = Object.freeze({
  Diff: "diff",
  Keepalive: "keepalive",
  State: "state",
  Manifest: "manifest",
  SnapshotPart: "snapshot-part",
  SnapshotEnd: "snapshot-end",
});

/** @param {import("@nats-io/nats-core").Msg} msg @param {string} name */
export function header(msg, name) {
  return msg.headers?.get(name) || undefined;
}

/** Reads the epoch/seq/prev/kind fields a presence frame's NATS headers carry, as the record shape `Glue` expects. */
export function frameEnvelope(msg) {
  const generation = header(msg, HEADER_GENERATION);
  const ownerRev = header(msg, HEADER_OWNER_REV);
  const ownerId = header(msg, HEADER_OWNER_ID);
  const kind = header(msg, HEADER_KIND);
  if (generation === undefined || ownerRev === undefined || ownerId === undefined || kind === undefined) {
    return undefined;
  }
  const seqRaw = header(msg, HEADER_SEQ);
  const prevRaw = header(msg, HEADER_PREV);
  return {
    kind,
    generation,
    owner_rev: ownerRev,
    owner_id: ownerId,
    seq: seqRaw === undefined ? undefined : Number(seqRaw),
    prev: prevRaw === undefined ? undefined : Number(prevRaw),
    snapshotId: header(msg, HEADER_SNAPSHOT_ID),
    requestId: header(msg, HEADER_REQUEST_ID),
    part: (() => {
      const raw = header(msg, HEADER_PART);
      return raw === undefined ? undefined : Number(raw);
    })(),
    parts: (() => {
      const raw = header(msg, HEADER_PARTS);
      return raw === undefined ? undefined : Number(raw);
    })(),
  };
}
