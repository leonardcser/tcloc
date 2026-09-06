# Releasing

## Versioning

The package version lives in `Cargo.toml`. Release tags use `v<version>` and must
match it exactly, for example `v0.1.1`. Each release needs a new version;
crates.io does not allow replacing published versions.

## Release

1. Bump the version in `Cargo.toml`.
2. Run `cargo check` to update `Cargo.lock`.
3. Commit both files and push the commit and matching tag:

   ```bash
   git add Cargo.toml Cargo.lock
   git commit -m "chore: release v0.1.1"
   git push origin main
   git tag v0.1.1
   git push origin v0.1.1
   ```

Pushing the tag triggers [the publish workflow](.github/workflows/publish.yml).
It automatically verifies the version, checks formatting, runs Clippy and tests,
verifies packaging, and publishes to crates.io using Trusted Publishing (OIDC).
No manual `cargo publish` is needed.

Check the workflow result in GitHub Actions to confirm the release succeeded.
