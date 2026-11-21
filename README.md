<p align="center">
  <img src="assets/logo-wordmark.png" alt="AstroDev" width="640">
</p>

<p align="center">
  <b>Agente de programación para tu terminal.</b> Local-first, open source y multiplataforma.<br>
  Funciona con modelos locales (Ollama, llama.cpp, LM Studio, vLLM) y con APIs externas.
</p>

<p align="center">
  <img alt="Licencia MIT" src="https://img.shields.io/badge/licencia-MIT-9B5CFF">
  <img alt="Rust" src="https://img.shields.io/badge/hecho%20con-Rust-FF4FD8">
  <img alt="Plataformas" src="https://img.shields.io/badge/Linux%20·%20macOS%20·%20Windows-3EC8FF">
</p>

---

> ⚠️ **En desarrollo activo.** Estado actual: **Fase 2** completada.

## Qué es

AstroDev es un agente tipo "Claude Code" que vive en tu terminal: lee tu proyecto,
busca, edita archivos con diffs, ejecuta comandos y itera hasta terminar la tarea.
Está pensado para funcionar **bien con modelos locales** en tu propio hardware.

## Estado de las fases

| Fase | Contenido | Estado |
|------|-----------|--------|
| 1 | TUI (ratatui), proveedor Ollama, herramientas básicas | ✅ |
| 2 | Bucle agente completo, permisos, sesiones, /undo, auto-resumen, tool-calling por prompt | ✅ |
| 3 | Detección de hardware + recomendador de modelos | ⏳ |
| 4 | Instalador de runtimes y modelos | ⏳ |
| 5 | APIs externas, keyring, perfiles y fallback | ⏳ |
| 6 | Onboarding, CI/releases, instaladores de una línea | ⏳ |

## Compilar y probar

Requisitos: Rust estable (1.80+) y, para chatear, [Ollama](https://ollama.com) con algún modelo.

```bash
cargo build --release
cargo test --workspace

ollama pull qwen2.5-coder:7b      # si aún no tienes un modelo
./target/release/astrodev          # abre la TUI en el proyecto actual
```

Otros usos:

```bash
astrodev -p "explica la estructura de este proyecto"   # sin TUI
astrodev models                                        # modelos del proveedor
astrodev tools                                         # herramientas disponibles
astrodev tool grep '{"pattern":"fn main"}'             # ejecutar una herramienta
astrodev tool edit_file '{"path":"a.rs","old_string":"x","new_string":"y"}' --dry-run
astrodev config init                                   # crea ~/.config/astrodev/config.toml
```

## Contexto del proyecto: `ASTRO.md`

Crea un `ASTRO.md` en la raíz de tu proyecto con convenciones, comandos de build/test y
cualquier cosa que el agente deba saber. Se incluye automáticamente en cada conversación.

## Arquitectura

Ver [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Privacidad

Cero telemetría. Los logs se guardan sólo en tu máquina (`--debug` para más detalle).

## Licencia

[MIT](LICENSE)
