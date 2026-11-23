# Configuración de AstroDev

Archivo: `config.toml` en el directorio de configuración (`astrodev config path` lo muestra).
Se crea en la primera ejecución. `ASTRODEV_CONFIG_DIR` permite usar otro directorio.

```toml
default_provider = "ollama"
default_model = "qwen2.5-coder:7b"
# catalog_url = "https://github.com/AstroSoftwareMoon/AstroCode/releases/latest/download/models.json"

# ---------- Proveedores ----------
[providers.ollama]
kind = "ollama"                     # ollama | openai | anthropic | google
base_url = "http://localhost:11434"
tool_mode = "auto"                  # auto | native | prompt

[providers.anthropic]
kind = "anthropic"
api_key_ref = "anthropic"           # entrada en el keyring (servicio "astrodev")
effort = "high"                     # opcional: low | medium | high | xhigh | max

[providers.groq]
kind = "openai"                     # cualquier API compatible con OpenAI
base_url = "https://api.groq.com/openai/v1"
api_key_ref = "groq"

[providers.llamacpp]
kind = "openai"
base_url = "http://127.0.0.1:8080/v1"

# ---------- Perfiles por tarea y fallback ----------
[profiles]
plan = "anthropic/claude-opus-5-5"  # primer paso de cada turno
edit = "ollama/qwen2.5-coder:14b"   # pasos con herramientas
summarize = "ollama/llama3.2:3b"    # resúmenes de contexto
fallback = ["groq/llama-3.3-70b-versatile"]

# ---------- Apariencia ----------
[ui]
theme = "mono"                      # mono (blanco y negro) | astro (a color); NO_COLOR fuerza mono

# ---------- Permisos ----------
[permissions]
mode = "ask"                        # ask | auto-project | auto-all
always_allow = ["run_command"]      # herramientas que nunca preguntan (salvo comandos peligrosos)
dangerous_patterns = ["terraform destroy"]  # regex extra que siempre piden confirmación

# ---------- Agente ----------
[agent]
max_steps = 25                      # iteraciones herramienta→modelo por turno
summarize_at = 0.8                  # fracción del contexto que dispara el resumen
default_context_window = 8192
temperature = 0.2                   # no se envía a Anthropic (su API la rechaza)
tool_parse_retries = 2              # reintentos de llamadas mal formadas (modo prompt)

# ---------- Ajustes por modelo (los escribe el instalador) ----------
[models."qwen2.5-coder:7b"]
context_window = 16384
gpu_layers = 999
measured_tps = 42.0

# ---------- Precios propios (USD por millón de tokens) ----------
[pricing."gpt-4.1"]
input = 2.0
output = 8.0
cached_input = 0.5
```

## Memoria (ASTRO.md)

No se configura en este archivo: AstroDev lee, en este orden, `ASTRO.md` en el directorio de
configuración (global), `ASTRO.md` en la raíz del proyecto (o `.astrodev/ASTRO.md`), los `ASTRO.md` de
las carpetas entre la raíz y el directorio actual, y `ASTRO.local.md` (personal). Una línea
`@ruta` importa otro archivo (relativo al que lo importa; `~/` para tu home; hasta 5 niveles).

## Claves de API

Nunca se guardan en este archivo. Orden de búsqueda:

1. `ASTRODEV_KEY_<NOMBRE>` (p. ej. `ASTRODEV_KEY_GROQ`).
2. La variable estándar del proveedor (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, `OPENROUTER_API_KEY`, `GROQ_API_KEY`, `DEEPSEEK_API_KEY`, `MISTRAL_API_KEY`).
3. El keyring del sistema (entrada `api_key_ref`), que es donde las guarda `/provider add`.

## Variables de entorno

| variable | uso |
|---|---|
| `ASTRODEV_CONFIG_DIR` | directorio de configuración y datos alternativo |
| `ASTRODEV_LOG` | filtro de logs (`debug`, `astrodev_core=trace`…) |
| `ASTRODEV_HW_OVERRIDE` | JSON de `astrodev hardware --json` para simular otro equipo |
| `ASTRODEV_KEY_<NOMBRE>` | clave de API de un proveedor |

## Datos y privacidad

Las sesiones (`sessions/`), el catálogo en caché, los modelos GGUF, los runtimes instalados y los
logs viven en el directorio de datos. AstroDev no envía telemetría: sólo se conecta a los proveedores
que configuras, a GitHub para actualizar el catálogo y a las fuentes de descarga que tú pides.
