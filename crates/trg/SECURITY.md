# Security

## Reporting a vulnerability

Follow the [TrogonStack security policy](https://github.com/TrogonStack/.github/security/policy). Do not open a public issue for a vulnerability.

## Verifying a download

Every `trg` release tarball and `sha256sums.txt` carries a signed build provenance attestation. Pin both the repository and the signing workflow, otherwise the check only proves that some GitHub workflow built the file:

```sh
gh attestation verify trg-<version>-<target>.tar.gz \
  --repo TrogonStack/rusty-monorepo \
  --signer-workflow TrogonStack/rusty-monorepo/.github/workflows/release-artifacts.yml
```

Then check the tarball against `sha256sums.txt`, which is attested the same way:

```sh
gh attestation verify sha256sums.txt \
  --repo TrogonStack/rusty-monorepo \
  --signer-workflow TrogonStack/rusty-monorepo/.github/workflows/release-artifacts.yml
sha256sum --check --ignore-missing sha256sums.txt
```

Each release also ships `packslip.trg.sigstore.json`, a signed manifest of every artifact, which `packslip install` and mise can verify.
