# AstroDev

Agente de programación para terminal (estilo Claude Code) en Rust: TUI con ratatui, modelos
locales (Ollama, llama.cpp, LM Studio, vLLM) y APIs (OpenAI-compatible, Anthropic, Gemini).
Binario único `astrodev`. Textos de cara al usuario en **español**.

## Comandos

```bash
cargo build                                   # compilar todo el workspace
cargo run -p astrodev                         # abrir la TUI en el directorio actual
cargo run -p astrodev -- -p "hola"            # un prompt sin TUI
cargo test --workspace                        # todos los tests (no necesitan red ni Ollama)
cargo test -p astrodev-core agent_mock        # un test concreto
cargo fmt --all                               # formato (rustfmt.toml: ancho 120)
cargo clippy --workspace --all-targets -- -D warnings
```

Para probar la TUI sin modelos reales hay que levantar un servidor que imite la API de
Ollama en `localhost:11434` y usar `ASTRODEV_CONFIG_DIR=/tmp/cfg` para no tocar tu configuración.
`ASTRODEV_HW_OVERRIDE=hw.json` simula otro hardware (formato de `astrodev hardware --json`).

## Arquitectura

| crate | responsabilidad |
|---|---|
| `crates/astrodev-core` | tipos, traits `Provider`/`Tool`, bucle agente (`agent.rs`), permisos, sesiones, checkpoints, contexto, memoria ASTRO.md, tool calling por prompt |
| `crates/astrodev-providers` | Ollama, OpenAI-compatible, Anthropic, Gemini, `MockProvider`, reintentos, keyring, precios |
| `crates/astrodev-tools` | herramientas del agente: archivos, grep/glob, list_dir, shell, git |
| `crates/astrodev-hardware` | detección de hardware, cálculo de memoria, catálogo (`catalog/models.json`), recomendador |
| `crates/astrodev-installer` | runtimes, descargas reanudables con SHA-256, test de humo y ajuste |
| `crates/astrodev-tui` | interfaz: `app.rs` (estado y teclas), `ui.rs` (dibujo), `*_view.rs` (modales) |
| `crates/astrodev` | binario: CLI con clap, logging, ensamblado |

- `astrodev-core` **no depende** de ningún otro crate del workspace (así el agente se prueba sin red).
- Flujo de un turno: `Agent::run_turn` → `Provider::chat_stream` → `StreamEvent`s → permisos
  (`permissions.rs`) → `Tool::execute` → resultado al historial → siguiente paso.
- La TUI recibe `AgentEvent`s por un canal; los permisos se responden con `PermissionRequest::respond`.

## Convenciones

- Errores con `thiserror` en librerías y `anyhow` en el binario. Mensajes con pista de solución.
- Toda ruta que use una herramienta pasa por `Project::resolve` (sandbox del proyecto).
- Toda herramienta nueva declara su `ToolEffect`; las que escriben implementan `preview` y `affected_paths`.
- Nada de telemetría ni llamadas de red no pedidas por el usuario.
- Tests junto al código (`#[cfg(test)]`); HTTP simulado con `wiremock`; agente con `MockProvider`.
- Commits en español, en imperativo, con el porqué en el cuerpo.

## Flujo de trabajo

1. Cambia lo mínimo y añade o ajusta tests.
2. `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`.
3. Si tocas la TUI, pruébala de verdad en una terminal (tmux) además de los tests.

## Cuidado con

- `catalog/models.json` está embebido en el binario (`include_str!`): si cambias su formato,
  actualiza `astrodev-hardware/src/catalog.rs` y sube `version`.
- Anthropic: no enviar `temperature` (la API 2026-10-21 la rechaza) y reenviar intactos los bloques
  de razonamiento (`Message::raw`).
- Las claves de API van al keyring o a variables de entorno, **nunca** a `config.toml`.
- Versión en `Cargo.toml` (workspace) = etiqueta de la release (`vX.Y.Z`); el workflow lo comprueba.
