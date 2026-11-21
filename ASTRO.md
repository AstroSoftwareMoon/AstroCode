# AstroDev — contexto para el agente

- Workspace Rust en `crates/`. Regla: `astrodev-core` no depende de otros crates del workspace.
- Compilar: `cargo build`. Tests: `cargo test --workspace`. Lint: `cargo clippy --workspace --all-targets`.
- Textos de cara al usuario en español. Sin telemetría.
- Toda ruta que use una herramienta debe pasar por `Project::resolve`.
