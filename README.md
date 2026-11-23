<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo-wordmark.png">
    <img src="assets/logo-wordmark-light.png" alt="AstroDev" width="560">
  </picture>
</p>

<p align="center">
  <b>Un agente de programación para tu terminal.</b> Local-first, open source y multiplataforma.<br>
  Funciona con modelos locales (Ollama, llama.cpp, LM Studio, vLLM) y con APIs (Claude, GPT, Gemini, OpenRouter, Groq, DeepSeek, Mistral).
</p>

<p align="center">
  <a href="https://github.com/AstroSoftwareMoon/AstroCode/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/AstroSoftwareMoon/AstroCode/actions/workflows/ci.yml/badge.svg"></a>
  <img alt="Licencia MIT" src="https://img.shields.io/badge/licencia-MIT-111111">
  <img alt="Rust" src="https://img.shields.io/badge/hecho%20con-Rust-111111">
  <img alt="Plataformas" src="https://img.shields.io/badge/Linux%20·%20macOS%20·%20Windows-111111">
  <img alt="Sin telemetría" src="https://img.shields.io/badge/telemetría-cero-111111">
</p>

<p align="center">
  <img src="docs/images/demo.gif" alt="Demo de AstroDev: explora el proyecto, propone un cambio con diff y lo aplica tras pedir permiso" width="900">
</p>

## Instalación

**Linux y macOS**

```bash
curl -fsSL https://github.com/AstroSoftwareMoon/AstroCode/releases/latest/download/install.sh | sh
```

**Windows (PowerShell)**

```powershell
irm https://github.com/AstroSoftwareMoon/AstroCode/releases/latest/download/install.ps1 | iex
```

Los instaladores descargan el binario de la última release, **verifican su SHA-256** y lo dejan en
`~/.local/bin` (o `%LOCALAPPDATA%\astrodev\bin`). No necesitan permisos de administrador.

¿Prefieres compilarlo? `cargo install --git https://github.com/AstroSoftwareMoon/AstroCode astrodev`

## Primeros pasos

```bash
cd tu-proyecto
astrodev
```

La primera vez se abre un asistente de 4 pasos que funciona sin leer documentación:

1. **Detecta tu hardware** (CPU, RAM, GPU/VRAM, disco).
2. **Eliges** modelos locales, una API, o ambos.
3. **Recomienda e instala** el mejor modelo que cabe en tu equipo (instala Ollama si hace falta, descarga el modelo, hace un test de humo y ajusta el contexto).
4. **Abre el proyecto** y, si quieres, crea un `ASTRO.md` con su contexto.

<p align="center"><img src="docs/images/onboarding.png" alt="Asistente de bienvenida: elegir e instalar un modelo" width="820"></p>

## Qué hace

### Un agente que trabaja en tu código

Lee, busca, edita archivos con diffs, ejecuta comandos y git, e itera hasta terminar.
Cada cambio se previsualiza y **tú decides**.

<p align="center"><img src="docs/images/hero.png" alt="AstroDev explorando un proyecto y editando un archivo" width="820"></p>

### Permisos claros

Tres modos (`Shift+Tab` para cambiar): **ask** (pregunta siempre), **auto-project** (ediciones
automáticas dentro del proyecto) y **auto-all**. Los comandos peligrosos (`rm -rf`, `git push --force`,
`curl … | sh`, `sudo`…) y cualquier acceso **fuera del proyecto** piden confirmación en todos los modos.

<p align="center"><img src="docs/images/permiso.png" alt="Diálogo de permiso con la vista previa del diff" width="820"></p>

### Sabe qué modelos te caben

`/hardware` calcula la memoria de cada modelo (pesos GGUF + KV cache según el contexto + margen),
estima los tokens/s por el ancho de banda de tu GPU o CPU y lo clasifica en
**recomendado / posible / demasiado grande**. El catálogo se actualiza desde este repositorio sin tocar el binario.

<p align="center"><img src="docs/images/hardware.png" alt="Pantalla de hardware y modelos recomendados" width="820"></p>
<p align="center"><sub>Capturas de <code>/hardware</code> con hardware simulado (RTX 4070 12 GB) mediante <code>ASTRODEV_HW_OVERRIDE</code>.</sub></p>

### Tus modelos y cualquier otro

En la misma pantalla, la pestaña **Instalados** lista lo que ya tienes (Ollama y GGUF de llama.cpp),
cuánto ocupa, si cabe bien en tu equipo y cuál está en uso; desde ahí lo usas o lo borras.
**Buscar** filtra todo el catálogo y, si el modelo no está, lo descarga igualmente con Ollama:
cualquier etiqueta de [ollama.com/library](https://ollama.com/library) (`deepseek-r1:8b`) o un GGUF de
Hugging Face (`hf.co/usuario/repo:Q4_K_M`).

<table>
  <tr>
    <td><img src="docs/images/instalados.png" alt="Modelos instalados"></td>
    <td><img src="docs/images/buscar.png" alt="Buscador de modelos"></td>
  </tr>
</table>

### Instala por ti

Instala Ollama o llama.cpp con la build correcta (CUDA, ROCm/HIP, Metal, Vulkan o CPU), descarga
modelos con barra de progreso y **reanudación**, verifica hashes, controla el espacio libre y, al acabar,
mide la velocidad y ajusta el contexto para que el modelo quede entero en la GPU.

<p align="center"><img src="docs/images/install.png" alt="Instalación de un modelo con barra de progreso" width="820"></p>

### Y además

- **Cualquier modelo**: Ollama, llama.cpp, LM Studio, vLLM, cualquier endpoint compatible con OpenAI, Anthropic, Google Gemini, OpenRouter, Groq, DeepSeek y Mistral. Añádelos con `/provider add`.
- **Claves seguras**: en el keyring del sistema (Keychain, Credential Manager, Secret Service), nunca en texto plano.
- **Modelos pequeños bien soportados**: si el modelo no tiene tool calling nativo, AstroDev usa un formato por prompt con un parser tolerante (repara JSON roto) y reintentos.
- **Perfiles por tarea** (planificar, editar, resumir) y **fallback** automático si un modelo falla.
- **Sesiones** guardadas y reanudables (`/resume`, `astrodev -c`), **`/undo`** que restaura archivos y conversación, **resumen automático** del contexto.
- **Streaming, cancelación (Esc), reintentos con backoff** y conteo de tokens y coste (`/cost`).
- **Minimalista**: tema en blanco y negro por defecto (`/theme astro` para la versión a color; respeta `NO_COLOR`).
- **Cero telemetría.** Los logs se quedan en tu máquina (`--debug` para más detalle).

## Comandos

| En la TUI | |
|---|---|
| `/model [proveedor/]modelo` | cambiar de modelo (sin argumento, lista los disponibles) |
| `/hardware` | modelos recomendados, instalados y buscador |
| `/memory` · `# nota` · `/init` | memoria del proyecto (ASTRO.md) |
| `/theme mono\|astro` | blanco y negro (por defecto) o a color |
| `/provider add · list · remove` | gestionar proveedores y APIs |
| `/install <modelo>` · `/uninstall <modelo>` | instalar (catálogo, etiqueta de Ollama o `hf.co/…`) y borrar |
| `/permissions` · `Shift+Tab` | modo de permisos |
| `/undo` · `/resume` · `/clear` | deshacer, reanudar sesión, nueva conversación |
| `/compact` · `/context` · `/cost` | contexto y coste |
| `/setup` · `/help` | asistente inicial, ayuda |

| En la terminal | |
|---|---|
| `astrodev -p "…"` | un único prompt sin TUI (permisos con `--mode`) |
| `astrodev -c` / `--resume <id>` | reanudar la última sesión o una concreta |
| `astrodev hardware` · `recommend` | hardware y modelos recomendados (`--json`) |
| `astrodev models` · `search <texto>` | modelos instalados y búsqueda en el catálogo |
| `astrodev pull <modelo>` · `rm` · `install ollama\|llamacpp` | instalar (también etiquetas de Ollama y `hf.co/…`) y borrar |
| `astrodev provider add\|list\|remove\|test` | proveedores |
| `astrodev sessions` · `catalog update` · `smoke-test` · `config path` | utilidades |

## Memoria: `ASTRO.md`

Como `CLAUDE.md` en Claude Code: archivos Markdown con lo que el agente debe saber (comandos,
arquitectura, convenciones…), que se añaden a cada conversación. Se combinan varios niveles, y el más
específico prevalece:

| archivo | para qué | ¿en git? |
|---|---|---|
| `~/.config/astrodev/ASTRO.md` | tus preferencias en todos los proyectos | — |
| `ASTRO.md` (raíz del proyecto) | instrucciones del equipo para este proyecto | sí |
| `subcarpeta/ASTRO.md` | reglas de una parte del código (se cargan al trabajar ahí) | sí |
| `ASTRO.local.md` | tus notas personales para este proyecto | no (se añade a `.gitignore`) |

- **`/init`** analiza el proyecto y escribe un `ASTRO.md` completo (comandos, arquitectura, convenciones…).
- **`# texto`** guarda una nota al momento: `# los tests se ejecutan con cargo nextest run`.
- **`/memory`** muestra qué se ha cargado; **`/memory edit proyecto|local|global`** lo abre en tu `$EDITOR`.
- **`@ruta/archivo.md`** en una línea importa otro archivo (p. ej. `@docs/estilo.md`).

Este repositorio tiene el suyo como ejemplo: [ASTRO.md](ASTRO.md).

<p align="center"><img src="docs/images/memoria.png" alt="Memoria: nota rápida y /memory" width="820"></p>

## Configuración

`~/.config/astrodev/config.toml` (Linux), `~/Library/Application Support/dev.astrodev.astrodev/` (macOS)
o `%APPDATA%\astrodev\astrodev\config\` (Windows). Mira la ruta con `astrodev config path` y todas las
opciones en [docs/CONFIG.md](docs/CONFIG.md).

## Arquitectura

Workspace de Rust con crates independientes: `core` (agente, permisos, sesiones), `providers`, `tools`,
`hardware`, `installer` y `tui`. Detalles en [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Contribuir

¡Bienvenido! Lee [CONTRIBUTING.md](CONTRIBUTING.md). Proponer modelos para el catálogo es tan fácil como
editar [`catalog/models.json`](catalog/models.json).

## Licencia

[MIT](LICENSE)
