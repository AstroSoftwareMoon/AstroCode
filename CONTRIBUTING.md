# Contribuir a AstroDev

¡Gracias por tu interés! Toda ayuda cuenta: código, modelos para el catálogo, traducciones,
documentación o simplemente avisar de un error.

## Preparar el entorno

```bash
git clone https://github.com/AstroSoftwareMoon/AstroCode
cd AstroCode
cargo build
cargo test --workspace
```

Requisitos: Rust estable (1.82 o superior). No hacen falta Ollama ni claves de API para los tests:
usan un proveedor falso y servidores HTTP simulados.

## Antes de abrir un PR

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

La CI ejecuta lo mismo en Linux, macOS y Windows.

## Estructura

| crate | qué contiene |
|---|---|
| `astrodev-core` | tipos, traits `Provider`/`Tool`, bucle agente, permisos, sesiones, checkpoints, contexto, tool calling por prompt |
| `astrodev-providers` | Ollama, OpenAI-compatible, Anthropic, Google, reintentos, keyring, precios |
| `astrodev-tools` | herramientas del agente (archivos, búsqueda, shell, git) |
| `astrodev-hardware` | detección de hardware, cálculo de memoria, catálogo, recomendador |
| `astrodev-installer` | runtimes, descargas reanudables, test de humo |
| `astrodev-tui` | interfaz ratatui |
| `astrodev` | binario y CLI |

`astrodev-core` no depende de ningún otro crate del workspace: así el agente se prueba sin red.

## Pautas

- **Seguridad primero.** Toda ruta que use una herramienta pasa por `Project::resolve`. Cualquier
  herramienta nueva con efectos debe declarar su `ToolEffect` correctamente.
- **Sin telemetría.** No se aceptan cambios que envíen datos fuera de la máquina sin acción explícita del usuario.
- **Textos de cara al usuario en español**, claros y con una pista de cómo resolver el problema.
- **Tests** para la lógica nueva: parsers, cálculos y flujos del agente (con `MockProvider`).
- Commits pequeños y descriptivos.

## Añadir un modelo al catálogo

Edita `catalog/models.json` (formato descrito en `catalog/README.md`), sube el campo `version` y
comprueba que `cargo test -p astrodev-hardware` pasa. Incluye la arquitectura real (capas, cabezas KV,
dimensión de cabeza): el recomendador la usa para calcular la KV cache.

## Añadir un proveedor

Implementa el trait `Provider` en `crates/astrodev-providers/src/` (mira `openai.rs` como ejemplo),
regístralo en `build()` y añade un preset en `presets.rs`. Prueba el streaming con `wiremock`.

## Informar de errores

Abre un issue con la versión (`astrodev --version`), el sistema operativo, los pasos para reproducirlo
y, si puedes, el log de `--debug` (en el directorio de datos, carpeta `logs/`). Para problemas de
hardware: la salida de `astrodev hardware --json`.

## Código de conducta

Trata a los demás con respeto. Las discusiones van sobre ideas y código, nunca sobre personas.
