## Description

<!-- Provide a brief description of the changes -->

## Type of Change

<!-- Mark the relevant option with an "x" -->

- [ ] Bug fix (non-breaking change fixing an issue)
- [ ] New feature (non-breaking change adding functionality)
- [ ] Breaking change (fix or feature causing existing functionality to change)
- [ ] Documentation update
- [ ] Refactoring (no feature change)
- [ ] Dependency update
- [ ] Performance improvement

## Related Issues

<!-- Link related issues: Fixes #123, Relates to #456 -->

Closes #

## Testing

<!-- Describe how you tested the changes -->

- [ ] Unit tests added/updated
- [ ] Integration tests added/updated
- [ ] Manual testing completed
- [ ] Test coverage maintained/improved

### Test Results

```
<!-- Paste test output here -->
```

## Quality Checklist

<!-- Verify all items before submitting -->

- [ ] Code follows project style guidelines (`cargo fmt --all`)
- [ ] All clippy lints pass with strict warnings (`cargo clippy --workspace --all-targets -- -D warnings`)
- [ ] All tests pass locally (`cargo test --workspace`)
- [ ] Documentation updated (if applicable)
- [ ] No unnecessary code changes (focused on the specific issue)
- [ ] Commit messages follow [conventional commits](https://www.conventionalcommits.org/)
- [ ] No credentials or secrets committed

## Performance Impact

<!-- Describe any performance implications -->

- [ ] No performance impact
- [ ] Performance improved (describe improvements)
- [ ] Performance regressed (describe and justify)

## Breaking Changes

<!-- Describe any breaking changes for users -->

- [ ] No breaking changes
- [ ] Breaking changes (describe below)

## Screenshots/Output (if applicable)

<!-- Include screenshots or command output if relevant -->

## Additional Notes

<!-- Any additional context or information -->

---

**Before merging, ensure:**
- ✅ All CI checks pass
- ✅ Code review completed
- ✅ All discussions resolved
- ✅ Changelog/version updated (if releasing)
