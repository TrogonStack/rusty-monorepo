# Use Google Cloud Secret Manager as a secrets backend

Read a secret from Secret Manager into an `[exec]` or MCP var, and mirror a value
into it from another backend, without `trg` holding any Google credential of
its own.

## Before you start

- The [`gcloud` CLI](https://cloud.google.com/sdk/gcloud) is installed and
  authenticated for the account you declare, with `gcloud auth login` or, in
  CI, `gcloud auth login --cred-file` (`google-github-actions/setup-gcloud` runs
  it). A credential file or access token that is only set in the environment is
  ignored, because it could name a different identity than `account`.
- The secret exists. `trg` adds versions to a secret and never creates one, so
  declare it where you declare the rest of your infrastructure.
- The identity can access versions (`roles/secretmanager.secretAccessor`) to
  read, and add versions (`roles/secretmanager.secretVersionAdder`) to write.

## 1. Declare the backend

```toml trg-example=fragment
[secrets.backends.gsm]
kind    = "gcp_secret_manager"
project = "my-project"
account = "deployer@my-project.iam.gserviceaccount.com"
```

`project` and `account` are both required, and neither is taken from
`gcloud config`, so the same file acts as the same identity on the same project
on every machine.

## 2. Check it

```sh
trg doctor --backend gsm
```

The report says whether `gcloud` answers, whether the account holds
credentials, and whether the project can be listed with it.

## 3. Read a secret

A secret is addressed by its id, as `secret`. It has no `path` or `key`:

```sh
trg secret get --backend gsm --secret deploy-token
```

```toml trg-example=fragment
[exec.deploy.env]
DEPLOY_TOKEN = { backend = "gsm", secret = "deploy-token" }
```

The `latest` version is read, byte for byte.

## 4. Write a version

```sh
op read "op://Engineering/deploy-key/token" | trg secret put --backend gsm --secret deploy-token
```

The value arrives on stdin and goes to `gcloud` on stdin, so it is never in a
process listing. To keep a value in sync from another backend instead, declare
a [mirror](mirror-a-secret-into-another-backend.md#mirror-a-secret-into-secret-manager).

## Troubleshooting

| Message | What to do |
| ------- | ---------- |
| `secret ... does not exist in project ...` | Create the secret first. `trg` never does. |
| `permission denied` | The account lacks the role above on that secret or project. |
| `gcloud ... timed out` | `gcloud` is probably waiting on a reauthentication prompt. Run `gcloud auth login`. |
