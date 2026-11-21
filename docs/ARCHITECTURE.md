# Arquitectura de AstroDev

AstroDev es un agente de programación para terminal escrito en Rust. Se distribuye como
un **binario único** (`astrodev`) y funciona con modelos locales (Ollama, llama.cpp,
LM Studio, vLLM) y con APIs externas (OpenAI-compatible, Anthropic, Google, OpenRouter,
Groq, DeepSeek, Mistral).

## Por qué Rust (y no TypeScript/Ink)

* Binario único sin runtime: instalar = descargar un archivo. Clave para el instalador de una línea.
* Detección de hardware y descargas de varios GB con reanudación/hash: Rust es ideal.
* `ratatui` + `crossterm` funcionan igual en Linux, macOS y Windows.
* Arranque instantáneo y memoria baja: deja la RAM/VRAM para el modelo local.

## Crates del workspace

```
crates/
├── astrodev-core        Tipos compartidos y lógica sin E/S de red
│   ├── message.rs       Message, Role, ContentPart, ToolCall, ToolResult
│   ├── provider.rs      trait Provider + ChatRequest/StreamEvent/Usage
│   ├── tool.rs          trait Tool + ToolSpec (JSON Schema) + ToolContext
│   ├── config.rs        Config TOML (~/.config/astrodev/config.toml)
│   ├── project.rs       Raíz del proyecto, ASTRO.md, sandbox de rutas
│   ├── permissions.rs   (F2) modos ask / auto-project / auto-all, comandos peligrosos
│   ├── agent.rs         (F2) bucle agente: plan → tool → observar → iterar
│   ├── session.rs       (F2) sesiones persistentes y reanudables
│   ├── checkpoint.rs    (F2) checkpoints para /undo
│   ├── context.rs       (F2) presupuesto de tokens y auto-resumen
│   └── prompt_tools.rs  (F2) tool calling por prompt (JSON/XML robusto + reintentos)
├── astrodev-providers   Implementaciones de Provider
│   ├── ollama.rs        (F1) /api/chat NDJSON streaming, tools nativas, /api/tags
│   ├── mock.rs          (F1) proveedor falso guionizado para tests
│   ├── openai.rs        (F5) OpenAI-compatible (llama.cpp, LM Studio, vLLM, OpenRouter, Groq, DeepSeek, Mistral)
│   ├── anthropic.rs     (F5) Messages API
│   ├── google.rs        (F5) Gemini
│   ├── retry.rs         (F5) backoff exponencial con jitter
│   ├── fallback.rs      (F5) perfiles por tarea + fallback automático
│   ├── pricing.rs       (F5) coste por token
│   └── keyring.rs       (F5) claves en el keyring del SO
├── astrodev-tools       Herramientas del agente
│   ├── fs.rs            read_file, write_file, edit_file (búsqueda/reemplazo + diff unificado)
│   ├── search.rs        grep (regex, respeta .gitignore), glob
│   ├── list.rs          list_dir (árbol del proyecto)
│   ├── shell.rs         run_command (timeout, cancelación, salida truncada)
│   └── git.rs           git status/diff/log/commit
├── astrodev-hardware    (F3) CPU/RAM/GPU/VRAM/disco, cálculo de memoria, catálogo, recomendador
├── astrodev-installer   (F4) Ollama/llama.cpp, descargas reanudables con SHA-256, test de humo
├── astrodev-tui         ratatui: chat, streaming, diffs, diálogos de permiso, /hardware, onboarding
└── astrodev             Binario: CLI (clap), logging, cableado de todo lo anterior
```

Regla de dependencias: `core` no depende de nadie del workspace. `providers`, `tools`,
`hardware` e `installer` dependen sólo de `core`. `tui` depende de todos; `astrodev` (bin)
los ensambla. Así el bucle agente se testea con `MockProvider` sin red.

## Flujo de una petición

```
 usuario ─▶ TUI ─▶ Agent::run(turno)
                     │  ChatRequest(messages, tools)
                     ▼
                 Provider::chat_stream ──▶ StreamEvent::{TextDelta, ToolCall, Usage, Done}
                     │                         (nativo o parseado de texto en modo prompt-tools)
                     ▼
              ¿tool calls? ─sí─▶ Permissions::check ─▶ (diff preview / confirmación en TUI)
                     │                                   ▼
                     │                              Tool::execute ─▶ ToolResult
                     │◀──────────────────────────────────┘ (se añade al historial e itera)
                     no
                     ▼
              respuesta final, checkpoint, guardar sesión
```

## Configuración

`~/.config/astrodev/config.toml` (en Windows `%APPDATA%\astrodev\config.toml`, en macOS
`~/Library/Application Support/astrodev/` — se resuelve con `directories`). Las claves de API
**nunca** van en el TOML: se guarda sólo una referencia al keyring.

```toml
default_provider = "ollama"
default_model = "qwen2.5-coder:7b"

[providers.ollama]
kind = "ollama"
base_url = "http://localhost:11434"

[permissions]
mode = "ask"          # ask | auto-project | auto-all
```

## Seguridad

* Todas las rutas se canonicalizan y se validan contra la raíz del proyecto (`project.rs`).
* Comandos peligrosos (`rm -rf`, `mkfs`, `dd`, `git push --force`, `curl | sh`…) piden
  confirmación **siempre**, incluso en modo auto-total.
* Cero telemetría. Logs locales en el directorio de datos; `--debug` sube el nivel.

## Fases

1. TUI + proveedor Ollama + herramientas básicas
2. Bucle agente, permisos, sesiones, checkpoints, slash commands, auto-resumen, prompt-tools
3. Hardware + recomendador + catálogo
4. Instalador (runtimes, descargas, test de humo, desinstalación)
5. APIs externas, keyring, perfiles y fallback, coste
6. Pulido: onboarding, CI/releases, instaladores de una línea, README, docs
