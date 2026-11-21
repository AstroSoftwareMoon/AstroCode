# Catálogo de modelos

`models.json` alimenta el recomendador de `/hardware`. AstroDev lleva una copia embebida
y descarga la más reciente de este repositorio con `astrodev catalog update` (o
automáticamente una vez al día), **sin necesidad de actualizar el binario**.

Campos por modelo:

| campo | significado |
|-------|-------------|
| `id` | identificador único (lo usa `astrodev pull <id>`) |
| `params_b` / `active_params_b` | parámetros totales / activos por token (MoE) en miles de millones |
| `layers`, `kv_heads`, `head_dim` | arquitectura, para calcular la KV cache según el contexto |
| `context_max` | contexto máximo admitido |
| `supports_tools` | tool calling nativo en Ollama/llama.cpp |
| `code_score` | puntuación orientativa para programación (0-100) |
| `quants[]` | cuantizaciones: `name`, `size_bytes`, `ollama_tag`, `gguf.repo`/`gguf.file` (Hugging Face) y `sha256` opcional |

Para proponer un modelo, abre un PR editando `models.json` (sube `version`).
