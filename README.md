# Forger

Agente de código modular escrito en Rust. Inspirado conceptualmente en
[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)
("todo es un plugin") y en el flujo de Claude Code / Cursor Agent / OpenCode.
**No es un fork** de ninguno de ellos.

El modelo (`Provider`), las herramientas (`Tool`), el sandbox, la UI y el
propio loop del agente se intercambian en tiempo de composición, sin tocar
`forger-core`.

## Crates

| Crate | Qué es |
|---|---|
| `forger-core` | `Message` / `StreamEvent`, traits `Provider` y `Tool`, `AgentLoop`, modo `quality` |
| `forger-sandbox` | Denylist de paths, resolución del path **final**, Landlock, timeout de comandos |
| `forger-providers` | `MockProvider` + `OpenAiCompatProvider` (DeepSeek, Ollama/llama.cpp compat, Mistral, …) |
| `forger-tools` | `read_file`, `list_dir`, `grep`, `edit_file`, `write_file`, `run_command` |
| `forger-cli` | binario `forger` — REPL y `--message` |
| `forger-server` | binario `forger-server` — UI local + SSE en loopback, **sin auth** |

## Uso

```sh
cargo run -p forger-cli -- --message "hola"
# MockProvider si no hay API key

export FORGER_API_KEY=...
export FORGER_BASE_URL=https://api.deepseek.com/v1
export FORGER_MODEL=deepseek-chat
cargo run -p forger-cli -- --message "lista los archivos del workspace"

cargo run -p forger-cli -- serve --port 7420
# solo 127.0.0.1 — ver SECURITY.md
```

`--yes` auto-aprueba herramientas sensibles. **No** salta la denylist de
`.env` / `.git` / `.ssh` / `credentials`. Eso requiere
`--allow-denied-paths` (segunda capa, confirmación distinta).

`--quality` corre 2 candidatos en paralelo (máximo 3) y un revisor los
puntúa 0–10. El merge es "el mejor completo", no un merge de diffs.

## Decisiones que no se reabren a la ligera

- Denylist de secretos: no negociable por default; dos capas independientes.
- `forger serve` en loopback, sin autenticación. Auth de verdad o nada.
- Riesgos aceptados y documentados en [`SECURITY.md`](SECURITY.md)
  (rename vía shell, Landlock `SCOPE_SIGNAL` en kernels < 6.12).

Apache-2.0. See [`LICENSE`](LICENSE).
