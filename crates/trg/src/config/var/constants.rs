/// What a var declaration may be, as the tail of the error for one that is not.
pub const VAR_SOURCE_EXPECTED: &str = "expected a string, or an inline table `{ env = \"NAME\", default = \"...\" }`, \
     `{ backend = \"NAME\", path = \"...\", key = \"...\" }`, \
     `{ backend = \"NAME\", ref = \"op://<vault>/<item>/<field>\" }`, \
     `{ backend = \"NAME\", secret = \"<secret-id>\" }` \
     or a secret table with `fallbacks = [ <secret tables>, ... ]`, \
     whose entries cannot carry `fallbacks` of their own";
