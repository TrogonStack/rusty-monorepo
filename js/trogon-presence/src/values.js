import { encodeUtf8Token, decodeUtf8Token, CodecError } from "./codec.js";

const KEY_MAX_RAW_BYTES = 256;
const KEY_MAX_ESCAPED_BYTES = 768;
const TOPIC_SEGMENT_MAX_RAW_BYTES = 128;
const TOPIC_SEGMENT_MAX_ESCAPED_BYTES = 384;
const TOPIC_MAX_RAW_BYTES = 256;
const TOPIC_MAX_ESCAPED_BYTES = 768;
const TOPIC_SEPARATOR = ":";
const TOKEN_SEPARATOR = ".";

function byteLength(text) {
  return new TextEncoder().encode(text).length;
}

export class ValueError extends Error {}

export class PresenceKey {
  /** @param {string} raw */
  constructor(raw) {
    if (raw.length === 0) {
      throw new ValueError("presence key is empty");
    }
    if (byteLength(raw) > KEY_MAX_RAW_BYTES) {
      throw new ValueError(`presence key is ${byteLength(raw)} bytes, the limit is ${KEY_MAX_RAW_BYTES}`);
    }
    const token = encodeUtf8Token(raw);
    if (token.length > KEY_MAX_ESCAPED_BYTES) {
      throw new ValueError(`presence key token is ${token.length} bytes, the limit is ${KEY_MAX_ESCAPED_BYTES}`);
    }
    this.raw = raw;
    this.token = token;
  }

  toString() {
    return this.raw;
  }

  static fromToken(token) {
    if (token.length > KEY_MAX_ESCAPED_BYTES) {
      throw new ValueError(`presence key token is ${token.length} bytes, the limit is ${KEY_MAX_ESCAPED_BYTES}`);
    }
    try {
      return new PresenceKey(decodeUtf8Token(token));
    } catch (error) {
      if (error instanceof CodecError) {
        throw new ValueError(`presence key token is not canonical: ${error.message}`);
      }
      throw error;
    }
  }
}

export class Topic {
  /** @param {string} raw */
  constructor(raw) {
    if (raw.length === 0) {
      throw new ValueError("topic is empty");
    }
    if (byteLength(raw) > TOPIC_MAX_RAW_BYTES) {
      throw new ValueError(`topic is ${byteLength(raw)} bytes, the limit is ${TOPIC_MAX_RAW_BYTES}`);
    }
    const segments = raw.split(TOPIC_SEPARATOR);
    const tokens = segments.map((segment, index) => {
      if (byteLength(segment) > TOPIC_SEGMENT_MAX_RAW_BYTES) {
        throw new ValueError(`topic segment ${index} is ${byteLength(segment)} bytes, the limit is ${TOPIC_SEGMENT_MAX_RAW_BYTES}`);
      }
      const token = encodeUtf8Token(segment);
      if (token.length > TOPIC_SEGMENT_MAX_ESCAPED_BYTES) {
        throw new ValueError(`topic segment ${index} is ${token.length} bytes, the limit is ${TOPIC_SEGMENT_MAX_ESCAPED_BYTES}`);
      }
      return token;
    });
    const joined = tokens.join(TOKEN_SEPARATOR);
    if (joined.length > TOPIC_MAX_ESCAPED_BYTES) {
      throw new ValueError(`topic is ${joined.length} bytes, the limit is ${TOPIC_MAX_ESCAPED_BYTES}`);
    }
    this.raw = raw;
    this.tokens = joined;
  }

  segments() {
    return this.raw.split(TOPIC_SEPARATOR);
  }

  toString() {
    return this.raw;
  }

  static fromTokens(tokens) {
    if (tokens.length > TOPIC_MAX_ESCAPED_BYTES) {
      throw new ValueError(`topic is ${tokens.length} bytes, the limit is ${TOPIC_MAX_ESCAPED_BYTES}`);
    }
    const parts = tokens.split(TOKEN_SEPARATOR);
    const segments = parts.map((token, index) => {
      let decoded;
      try {
        decoded = decodeUtf8Token(token);
      } catch (error) {
        if (error instanceof CodecError) {
          throw new ValueError(`topic token ${index} is not canonical: ${error.message}`);
        }
        throw error;
      }
      if (decoded.includes(TOPIC_SEPARATOR)) {
        throw new ValueError(`topic token ${index} decodes to a segment containing the separator`);
      }
      return decoded;
    });
    return new Topic(segments.join(TOPIC_SEPARATOR));
  }
}

const OPAQUE_ID_ENCODED_LEN = 22;
const OPAQUE_ID_PATTERN = /^[A-Za-z0-9_-]{22}$/;

export class HolderId {
  /** @param {string} raw canonical base64url opaque id, 16 random bytes encoded unpadded */
  constructor(raw) {
    if (!OPAQUE_ID_PATTERN.test(raw)) {
      throw new ValueError(`holder id must be ${OPAQUE_ID_ENCODED_LEN} canonical base64url characters`);
    }
    this.raw = raw;
  }

  toString() {
    return this.raw;
  }
}

export class ConnectionId {
  /** @param {string} raw canonical base64url opaque id, 16 random bytes encoded unpadded */
  constructor(raw) {
    if (!OPAQUE_ID_PATTERN.test(raw)) {
      throw new ValueError(`connection id must be ${OPAQUE_ID_ENCODED_LEN} canonical base64url characters`);
    }
    this.raw = raw;
  }

  toString() {
    return this.raw;
  }
}

export class Milliseconds {
  /** @param {number} value */
  constructor(value) {
    if (!Number.isFinite(value) || value < 0) {
      throw new ValueError("duration must be a non-negative finite number of milliseconds");
    }
    this.value = value;
  }

  static seconds(value) {
    return new Milliseconds(value * 1000);
  }
}
