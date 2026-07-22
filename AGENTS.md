# Agent Rules

1. Every code change must be validated with `cargo run`.
2. Every code change must be validated with `cargo test`.
3. GitHub Pages web builds must be saved in the repository-root `web_dist/` directory.
4. Changes to measured hot paths must follow `docs/performance.md`: record a before/after benchmark on the same hardware and update the baseline table when the accepted baseline changes.
