/// Directory, relative to a run directory, used as `HOME` under
/// [`super::EnvironmentPolicy::Isolated`].
pub const RUN_HOME_DIR_NAME: &str = "home";

/// Variables without which a command line tool cannot be expected to start.
pub const PROCESS_VARS: &[&str] = &[
    "PATH", "HOME", "TMPDIR", "SHELL", "USER", "LOGNAME", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "TZ",
];

/// Variables that decide whether the harness can reach its model provider over the
/// network at all: proxies, and the trust stores that proxies are usually paired with.
pub const NETWORK_VARS: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    "REQUESTS_CA_BUNDLE",
];

/// Prefixes for the cloud credentials a harness needs when it is pointed at a model
/// hosted on a cloud provider rather than at its vendor's own endpoint.
pub const CLOUD_CREDENTIAL_PREFIXES: &[&str] = &["AWS_", "GOOGLE_", "GCLOUD_", "AZURE_"];
