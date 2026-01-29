# Security Policy

## Reporting a Vulnerability

**Do not** open a public GitHub issue for security vulnerabilities.

Instead, please email security@trogonstack.dev with:

1. Description of the vulnerability
2. Steps to reproduce
3. Potential impact
4. Suggested fix (if available)

We will acknowledge receipt within 48 hours and work toward a fix.

## Security Practices

### Code Quality

- All code must pass `cargo clippy` with `-D warnings`
- All code must be formatted with `cargo fmt`
- All code requires tests
- Security audit runs on every PR

### Dependencies

- Daily dependency updates via Dependabot
- Automatic security vulnerability scanning
- Production dependencies separated from development
- Regular `cargo audit` checks

### GitHub Actions

- All workflows use pinned action versions
- Minimal permissions per workflow (principle of least privilege)
- Secrets managed through GitHub
- No credentials in logs or artifacts

## Security Checklist for Contributors

Before committing:

- [ ] No hardcoded credentials, API keys, or secrets
- [ ] No unsafe code without justification
- [ ] Proper input validation
- [ ] Error handling doesn't leak sensitive information
- [ ] No dependencies with known vulnerabilities (`cargo audit`)
- [ ] Code reviewed by maintainers

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.1.x   | ✅ Current development |

## Security Tools

### Cargo Audit

Regular security audits of dependencies:

```bash
cargo audit
```

### Clippy

Linting includes security checks:

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

### SARIF Upload

Security findings are uploaded to GitHub Security tab through CI.

## Vulnerability Disclosure Timeline

1. **Report received**: We acknowledge within 48 hours
2. **Investigation**: We investigate and assess severity
3. **Fix development**: We develop and test a patch
4. **Release**: We release a patched version
5. **Disclosure**: We publish security advisory

## Responsible Disclosure

We practice responsible disclosure and ask that researchers:

- Give us reasonable time to fix issues before public disclosure
- Do not test on production systems
- Do not access other users' data
- Do not perform denial-of-service attacks
- Keep vulnerability details confidential until fixed

## Questions?

For security questions or to report vulnerabilities, email security@trogonstack.dev

For general security discussions, use GitHub Security Advisories.
