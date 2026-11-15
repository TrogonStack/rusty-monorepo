use std::time::Duration;

/// The name `trg` answers to when git runs it as the signing program.
pub const PROGRAM_NAME: &str = "trg-git-ssh-sign";

pub const SSH_KEYGEN: &str = "ssh-keygen";

pub const SIGNATURE_SUFFIX: &str = ".sig";

pub const TRANSIT_KEY_TYPE: &str = "ed25519";

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
