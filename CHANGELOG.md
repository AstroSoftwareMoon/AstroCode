# Cambios

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
