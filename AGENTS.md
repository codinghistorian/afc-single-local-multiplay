# Agent Rules

1. Every code change must be validated with `cargo run`.
2. Every code change must be validated with `cargo test`.
3. GitHub Pages/Itch.io web builds must be saved in the repository-root `web_dist/` directory.
4. Changes to measured hot paths must follow `docs/performance.md`: record a before/after benchmark on the same hardware and update the baseline table when the accepted baseline changes.
5. Don't build web builds unless explicitly requested during the development process. A request to develop, test, package, or release the web client counts as explicit authorization.
6. Browser/Itch.io is the only product-client focus. Do not add or expand native or Steam client features unless the user explicitly requests them or a shared deterministic/networking contract requires compatibility work.
7. Every web multiplayer change must be game-tested against `afc-web-server` running in OrbStack with at least two isolated browser clients. A release candidate must exercise one through four isolated clients, complete a real battle, show the authority-confirmed result, return every participant to the same room, and complete a rematch.
8. Browser multiplayer QA must use real UI and gameplay input, inspect every client, and retain screenshots, browser console/network logs, server logs, metrics, room revisions, and result identities under ignored `target/qa/web/` paths or CI artifacts.
