# Cambios

## v0.2.0 — 2026-11-23

- **Modelos instalados y buscador** en `/hardware`: pestañas Recomendados · Instalados · Buscar.
  Instalados muestra tamaño, si cabe y cuál está en uso, y permite usarlo o borrarlo. El buscador filtra
  el catálogo y descarga con Ollama cualquier otro modelo (`deepseek-r1:8b`, `hf.co/…`).
  En la CLI: `astrodev models` (también GGUF), `astrodev search <texto>` y `astrodev pull <etiqueta>`.
- **Memoria al estilo CLAUDE.md**: `ASTRO.md` global, de proyecto, de subcarpetas y `ASTRO.local.md`
  personal; importaciones con `@ruta`; nota rápida con `# texto`; `/memory` y `/memory edit`; `/init`
  genera un ASTRO.md más completo. La raíz del proyecto es ahora el repositorio git más cercano.
- **Aspecto minimalista en blanco y negro**: tema `mono` por defecto (`/theme astro` para el de color,
  `[ui] theme` en la configuración, respeta `NO_COLOR`) y logo nuevo en blanco y negro.
- La barra de progreso de las descargas muestra el texto aparte, legible con cualquier tema.

## v0.1.1 — 2026-11-22

- Las conexiones HTTPS usan también los certificados raíz del sistema: AstroDev funciona detrás de
  proxies corporativos o antivirus que inspeccionan HTTPS (antes fallaban, p. ej., `catalog update`).

## v0.1.0 — 2026-11-22

Primera versión.

- Agente de programación en terminal (TUI con ratatui) con herramientas de archivos, búsqueda, shell y git.
- Permisos en tres modos con vista previa de diffs; comandos peligrosos y acceso fuera del proyecto siempre confirmados.
- Sesiones reanudables, `/undo` con checkpoints, resumen automático del contexto, tool calling por prompt.
- Proveedores: Ollama, OpenAI-compatible (llama.cpp, LM Studio, vLLM, OpenRouter, Groq, DeepSeek, Mistral), Anthropic y Google.
- Claves en el keyring del sistema, reintentos con backoff, perfiles por tarea, fallback y coste.
- Detección de hardware, recomendador de modelos y catálogo actualizable.
- Instalador de Ollama/llama.cpp y de modelos con descargas reanudables, verificación SHA-256 y test de humo.
- Asistente de primera ejecución e instaladores de una línea.
