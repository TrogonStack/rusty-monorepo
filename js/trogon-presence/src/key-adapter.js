const PREFIX = "p-";
const UPPER_HEX_DIGITS = "0123456789ABCDEF";

/** @param {string} rawKey */
export function toSafeKey(rawKey) {
  const bytes = new TextEncoder().encode(rawKey);
  let out = PREFIX;
  for (const byte of bytes) {
    out += "=" + UPPER_HEX_DIGITS[byte >> 4] + UPPER_HEX_DIGITS[byte & 0x0f];
  }
  return out;
}

export class KeyAdapterError extends Error {}

function hexValue(digit) {
  if (digit >= 0x30 && digit <= 0x39) return digit - 0x30;
  if (digit >= 0x41 && digit <= 0x46) return digit - 0x41 + 10;
  return undefined;
}

/** @param {string} safeKey */
export function fromSafeKey(safeKey) {
  if (!safeKey.startsWith(PREFIX)) {
    throw new KeyAdapterError(`${JSON.stringify(safeKey)} is missing the ${PREFIX} marker`);
  }
  const body = safeKey.slice(PREFIX.length);
  const codeUnits = [];
  for (let i = 0; i < body.length; ) {
    if (body[i] !== "=" || i + 2 >= body.length + 1) {
      throw new KeyAdapterError(`${JSON.stringify(safeKey)} has a malformed escape at offset ${i}`);
    }
    const highChar = body.charCodeAt(i + 1);
    const lowChar = body.charCodeAt(i + 2);
    const high = hexValue(highChar);
    const low = hexValue(lowChar);
    if (high === undefined || low === undefined) {
      throw new KeyAdapterError(`${JSON.stringify(safeKey)} has a malformed escape at offset ${i}`);
    }
    codeUnits.push((high << 4) | low);
    i += 3;
  }
  return new TextDecoder("utf-8", { fatal: true }).decode(new Uint8Array(codeUnits));
}

/** @param {Record<string, unknown>} obj @returns {Record<string, unknown>} */
export function mapToSafeKeys(obj) {
  const out = Object.create(null);
  for (const key of Object.getOwnPropertyNames(obj)) {
    out[toSafeKey(key)] = obj[key];
  }
  return out;
}

/** @param {Record<string, unknown>} obj @returns {Record<string, unknown>} */
export function mapFromSafeKeys(obj) {
  const out = Object.create(null);
  for (const key of Object.getOwnPropertyNames(obj)) {
    out[fromSafeKey(key)] = obj[key];
  }
  return out;
}
