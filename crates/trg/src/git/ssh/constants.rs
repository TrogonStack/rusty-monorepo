use std::time::Duration;

/// The name `trg` answers to when git runs it as the signing program.
pub const PROGRAM_NAME: &str = "trg-git-ssh-sign";

pub const SSH_KEYGEN: &str = "ssh-keygen";

pub const SIGNATURE_SUFFIX: &str = ".sig";

pub const SSHSIG_MAGIC: &[u8; 6] = b"SSHSIG";
pub const SSHSIG_VERSION: u32 = 1;
pub const ED25519_SIGNATURE_LEN: usize = 64;
pub const SIGNATURE_TYPE: &str = "ssh-ed25519";

pub const ARMOR_BEGIN: &str = "-----BEGIN SSH SIGNATURE-----";
pub const ARMOR_END: &str = "-----END SSH SIGNATURE-----";
pub const ARMOR_COLUMNS: usize = 70;

pub const AGENT_REQUEST_IDENTITIES: u8 = 11;
pub const AGENT_IDENTITIES_ANSWER: u8 = 12;
pub const AGENT_MAX_ANSWER_LEN: usize = 1 << 20;
pub const AGENT_IO_TIMEOUT: Duration = Duration::from_secs(10);

pub const RSA_SIGNATURE_TYPE: &str = "rsa-sha2-512";

pub const TRANSIT_RSA_HASH: &str = "sha2-512";
pub const TRANSIT_RSA_SCHEME: &str = "pkcs1v15";

pub const TRANSIT_ED25519: &str = "ed25519";
pub const TRANSIT_RSA_PREFIX: &str = "rsa-";

pub const DER_INTEGER: u8 = 0x02;
pub const DER_BIT_STRING: u8 = 0x03;
pub const DER_NULL: u8 = 0x05;
pub const DER_OID: u8 = 0x06;
pub const DER_SEQUENCE: u8 = 0x30;
pub const DER_LONG_FORM: u8 = 0x80;
pub const RSA_ENCRYPTION_OID: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
pub const PEM_FENCE: &str = "-----";
