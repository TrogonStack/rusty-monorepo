const ESCAPE_MARKER = 0x3d;
const EMPTY_TOKEN = "=";
const UPPER_HEX_DIGITS = "0123456789ABCDEF";

function passesThrough(byte) {
  return (
    (byte >= 0x30 && byte <= 0x39) ||
    (byte >= 0x41 && byte <= 0x5a) ||
    (byte >= 0x61 && byte <= 0x7a) ||
    byte === 0x5f ||
    byte === 0x2d
  );
}

/** @param {Uint8Array} bytes */
export function encodeToken(bytes) {
  if (bytes.length === 0) {
    return EMPTY_TOKEN;
  }
  let out = "";
  for (const byte of bytes) {
    if (passesThrough(byte)) {
      out += String.fromCharCode(byte);
    } else {
      out += "=" + UPPER_HEX_DIGITS[byte >> 4] + UPPER_HEX_DIGITS[byte & 0x0f];
    }
  }
  return out;
}

export class CodecError extends Error {}

function upperHexValue(digit) {
  if (digit >= 0x30 && digit <= 0x39) return digit - 0x30;
  if (digit >= 0x41 && digit <= 0x46) return digit - 0x41 + 10;
  return undefined;
}

/** @param {string} token @returns {Uint8Array} */
export function decodeToken(token) {
  if (token === EMPTY_TOKEN) {
    return new Uint8Array(0);
  }
  if (token.length === 0) {
    throw new CodecError("token is empty");
  }
  const bytes = new TextEncoder().encode(token);
  const out = [];
  let offset = 0;
  while (offset < bytes.length) {
    const byte = bytes[offset];
    if (passesThrough(byte)) {
      out.push(byte);
      offset += 1;
      continue;
    }
    if (byte !== ESCAPE_MARKER) {
      throw new CodecError(`byte ${byte.toString(16)} at offset ${offset} is outside the token alphabet`);
    }
    const high = bytes[offset + 1];
    const low = bytes[offset + 2];
    if (high === undefined || low === undefined) {
      throw new CodecError(`escape at offset ${offset} is truncated`);
    }
    const highValue = upperHexValue(high);
    const lowValue = upperHexValue(low);
    if (highValue === undefined || lowValue === undefined) {
      throw new CodecError(`escape at offset ${offset} is not two uppercase hex digits`);
    }
    const decoded = (highValue << 4) | lowValue;
    if (passesThrough(decoded)) {
      throw new CodecError(`escape at offset ${offset} encodes an in-alphabet byte`);
    }
    out.push(decoded);
    offset += 3;
  }
  const result = new Uint8Array(out);
  if (encodeToken(result) !== token) {
    throw new CodecError("token does not re-encode to itself");
  }
  return result;
}

export function encodeUtf8Token(text) {
  return encodeToken(new TextEncoder().encode(text));
}

export function decodeUtf8Token(token) {
  return new TextDecoder("utf-8", { fatal: true }).decode(decodeToken(token));
}
