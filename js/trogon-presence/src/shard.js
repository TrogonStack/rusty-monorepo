const FNV1A64_OFFSET_BASIS = 0xcbf29ce484222325n;
const FNV1A64_PRIME = 0x100000001b3n;
const MASK64 = 0xffffffffffffffffn;
const SHARD_COUNT_MIN = 64;
const SHARD_COUNT_MAX = 1024;
const SHARD_TOKEN_PREFIX = "s";

/** @param {Uint8Array} bytes */
export function fnv1a64(bytes) {
  let hash = FNV1A64_OFFSET_BASIS;
  for (const byte of bytes) {
    hash = ((hash ^ BigInt(byte)) * FNV1A64_PRIME) & MASK64;
  }
  return hash;
}

export class ShardError extends Error {}

function isPowerOfTwo(count) {
  return count > 0 && (count & (count - 1)) === 0;
}

export class ShardCount {
  /** @param {number} count */
  constructor(count) {
    if (!isPowerOfTwo(count) || count < SHARD_COUNT_MIN || count > SHARD_COUNT_MAX) {
      throw new ShardError(`shard count ${count} must be a power of two between 64 and 1024`);
    }
    this.count = count;
  }

  static default() {
    return new ShardCount(SHARD_COUNT_MIN);
  }

  tokenWidth() {
    return String(this.count - 1).length;
  }

  /** @param {Uint8Array} bytes */
  maskedIndex(bytes) {
    const index = fnv1a64(bytes) & BigInt(this.count - 1);
    return Number(index);
  }

  /** @param {number} index */
  token(index) {
    if (index < 0 || index >= this.count) {
      throw new ShardError(`shard ${index} is out of range for ${this.count} shards`);
    }
    return SHARD_TOKEN_PREFIX + String(index).padStart(this.tokenWidth(), "0");
  }

  /** @param {string} token */
  parseToken(token) {
    if (!token.startsWith(SHARD_TOKEN_PREFIX)) {
      throw new ShardError(`shard token ${JSON.stringify(token)} is malformed for this shard count`);
    }
    const digits = token.slice(SHARD_TOKEN_PREFIX.length);
    if (digits.length !== this.tokenWidth() || !/^[0-9]+$/.test(digits)) {
      throw new ShardError(`shard token ${JSON.stringify(token)} is malformed for this shard count`);
    }
    const index = Number.parseInt(digits, 10);
    if (index >= this.count) {
      throw new ShardError(`shard ${index} is out of range for ${this.count} shards`);
    }
    return index;
  }
}

/** @param {string} rawTopic the raw, unescaped topic string */
export function viewShardIndex(rawTopic, count) {
  return count.maskedIndex(new TextEncoder().encode(rawTopic));
}

/** @param {string} rawKey the raw, unescaped PresenceKey string */
export function writerShardIndex(rawKey, count) {
  return count.maskedIndex(new TextEncoder().encode(rawKey));
}
