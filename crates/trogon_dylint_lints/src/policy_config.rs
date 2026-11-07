use rustc_session::Session;

#[derive(Clone, Copy, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct PolicyConfig {
    profile: Profile,
}

#[derive(Clone, Copy, Default, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Profile {
    #[default]
    Trogon,
    Portable,
}

impl PolicyConfig {
    pub(crate) fn load(sess: &Session) -> Self {
        match dylint_linting::config::<Self>("trogon_dylint_lints") {
            Ok(config) => config.unwrap_or_default(),
            Err(error) => {
                let detail = error
                    .to_string()
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                sess.dcx()
                    .fatal(format!("invalid trogon_dylint_lints configuration: {detail}"))
            }
        }
    }

    pub(crate) fn uses_trogon_adapters(self) -> bool {
        matches!(self.profile, Profile::Trogon)
    }
}
