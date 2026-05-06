# Versioning

`lay` uses the public commit number as the patch version:

```text
0.1.<public-commit-number>
```

Git does not store reliable "push event" history in the repository, so the
project treats each pushed commit as the version step.

Current publication branch version:

- `0.1.116`

Do not rely on stale commit counts from this file. Before publishing or pushing,
run the bump script or verify the version fields manually.

Before each push:

```bash
bash scripts/bump-version-from-git.sh
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo build --release --bins
```

The version must be updated in:

- `Cargo.toml`
- `Cargo.lock`
- `extension/lay@radislabus-star.github.io/metadata.json`
- `extension/lay@radislabus-star.github.io/lay-impl.js`
