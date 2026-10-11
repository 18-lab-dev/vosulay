<!-- Thanks for contributing! See CONTRIBUTING.md for setup and the verification tiers. -->

## Summary

<!-- What does this change, and why? -->

Fixes #<!-- issue number, if any -->

## Verification

<!-- Tick what you actually ran; leave the rest unticked. Note OS, game and backend versions for manual checks. -->

- [ ] Tier 1: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`, `cargo test --workspace --all-features --locked`
- [ ] `python3 tools/notices/generate.py --check` (run `generate.py` after dependency changes)
- [ ] Tier 2: Windows cross-Clippy with cargo-xwin
- [ ] Tier 3: editor preview (`tools/manager-preview.py`) or the private CEF smoke
- [ ] Tier 4: manual in-game check — platform / game / backend:

## Checklist

- [ ] Tests added or updated for behavior changes
- [ ] `docs/` and README updated where behavior or setup changed
- [ ] Commit subjects follow `feat: …`, `fix: …`, `docs: …`
