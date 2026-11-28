/// The key written next to the value, naming where it came from.
pub const SOURCE_KEY: &str = "source";

pub const OPENSSH_MAGIC: &[u8] = b"openssh-key-v1\0";
pub const OPENSSH_FENCE_BEGIN: &str = "-----BEGIN OPENSSH PRIVATE KEY-----";
pub const OPENSSH_FENCE_END: &str = "-----END OPENSSH PRIVATE KEY-----";
pub const OPENSSH_CIPHER_NONE: &[u8] = b"none";
pub const SSH_TYPE_ED25519: &[u8] = b"ssh-ed25519";
pub const SSH_TYPE_RSA: &[u8] = b"ssh-rsa";
pub const ED25519_SEED_LEN: usize = 32;
pub const ED25519_PRIVATE_LEN: usize = 64;

/// `PrivateKeyInfo` for Ed25519 up to the seed: version 0, algorithm
/// `1.3.101.112`, and an OCTET STRING wrapping the seed's OCTET STRING.
pub const ED25519_PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];
