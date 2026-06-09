# Releasing

## Versioning

Aiki uses [semver](https://semver.org/): `major.minor.patch` (e.g., `v0.1.0`).

- **Patch** (`0.1.x`): Bug fixes, doc tweaks
- **Minor** (`0.x.0`): New features, non-breaking changes
- **Major** (`x.0.0`): Breaking changes

## Version Source of Truth

The version in `cli/Cargo.toml` is the single source of truth. It is used for:
- `aiki --version`
- release artifact naming
- `<aiki version="...">` values in agent metadata

## New Release Flow (Homebrew + GitHub Releases)

From the repo root (`/Users/glasner/code/aiki`):

1. Bump the version in `cli/Cargo.toml`.
2. Rebuild/check locally as needed:
   ```bash
   cargo build
   aiki doctor --fix
   ```
3. Commit your changes.
4. Push a version tag:
   ```bash
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

A push to a version tag triggers:
- GitHub Actions release workflow
- Cross-platform artifacts for GitHub Releases
- Auto-updated Homebrew formula in `glasner/homebrew-tap`

> Note: you do **not** manually run `gh release create` anymore.

## GitHub Release Notes

`dist` publishes an auto-generated body.

- Title/body comes from `cargo-dist`
- Homebrew install command is embedded automatically

## Optional local validation

Before cutting a real release, run:

```bash
cd cli
~/.cargo/bin/dist plan
```

This prints the planned release matrix and artifact set.
