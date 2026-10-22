const RANDOM_ID_BYTES = 16;

function randomIdBytes() {
  const bytes = new Uint8Array(RANDOM_ID_BYTES);
  globalThis.crypto.getRandomValues(bytes);
  return bytes;
}

function base64UrlEncode(bytes) {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** A 16-byte random id, base64url-encoded with no padding: the scheme RequestId, LocalViewId and ConnectionId share on the Rust side. */
export function generateRandomId() {
  return base64UrlEncode(randomIdBytes());
}
