# Editor integration

Sparkles formats and lints SPARQL, Turtle, TriG, N-Triples, N-Quads, JSON-LD, and SHACL Compact Syntax inside text editors in two ways: through `sparkles lsp` (a Language Server Protocol server over stdio) or by piping buffers through `sparkles fmt --stdin-filepath`. Both modes require the `sparkles` binary on `PATH`, built with the default `fmt` feature, and both read the nearest `.sparklesfmt.toml` so editor formatting always matches the CLI and CI.

| Mode | Capabilities | Best for |
|---|---|---|
| Language server (`sparkles lsp`) | Formatting, live syntax and lint diagnostics, safe quick fixes, and prefix completion. | Daily editing in Neovim, Helix, or any LSP-capable editor. |
| Formatter command (`sparkles fmt`) | Format-on-save via stdin/stdout or in-place `--write`, respecting `.sparklesfmtignore`. | Lightweight format-only plugins such as conform.nvim, apheleia, or VS Code Run on Save. |

## The language server

`sparkles lsp` communicates over stdio (its only transport; `--stdio` is accepted as a no-op). It exits with status `0` after `shutdown` and `exit`, or status `1` if the client disconnects without `shutdown`.

### Capabilities

* **Minimal formatting edits.** `textDocument/formatting` returns a single edit spanning only changed lines so undo history, folds, and marks elsewhere survive, returning no edits when a file is already formatted. `textDocument/rangeFormatting` formats the entire document because surrounding declarations affect layout.
* **Live diagnostics.** On open and on every change, the server publishes diagnostics from `sparkles fmt` and `sparkles lint`, clearing them when fixed or when the document closes. Syntax errors (`syntax`) prevent formatting without returning an LSP error because the diagnostic already explains the failure; warnings do not block formatting. `sparkles fmt` reports `unsafe-format` or `unstable-format` (internal safety-check warnings worth reporting), `config` (broken `.sparklesfmt.toml`), `comment-moved` (warning), `undeclared-prefix` (information), and `option-not-implemented` (unsupported config key, placed at line 1). `sparkles lint` publishes rule codes (`unused-prefix`, `cartesian-product`, and others) using severities from the `[lint]` table of `.sparklesfmt.toml`, including embedded SPARQL inside `sh:select`, `sh:ask`, `sh:construct`, and `sh:update` literals. Its `undefined-prefix` error supersedes the formatter's `undeclared-prefix` note.
* **Quick fixes.** Safe lint fixes (`unused-prefix`, `language-tag-case`, `redundant-datatype`) appear as `quickfix` code actions, and `source.fixAll.sparkles` applies all verified safe fixes at once. Known prefixes flagged by `undefined-prefix` also offer an individual quick fix to insert the declaration, though `source.fixAll.sparkles` skips these because the IRI comes from configuration rather than the document.
* **Configuration and language detection.** All formatting, lint, and prefix options come from the nearest `.sparklesfmt.toml`, re-read on every request so changes apply immediately; unsaved buffers without a path use defaults. Language detection checks the editor language id (`sparql`, `turtle`, `ttl`, `trig`, `ntriples`, `nquads`, `jsonld`, `json-ld`, `shaclc`), then file extension, then file content. Unsupported languages return `"… formatting is not available yet"` with no diagnostics.
* **Positions and line endings.** Positions default to UTF-16 code units or use UTF-8 byte offsets when the client negotiates `utf-8`. Input accepts `\n`, `\r\n`, and `\r`; formatted output always uses `\n`.

### Prefixes

In SPARQL, Turtle, and TriG documents, `sparkles lsp` completes known prefixes (`textDocument/completion`). After `PREFIX` or `@prefix`, it offers full undeclared prefix declarations (with trailing ` .` after `@prefix`). At the start of a prefixed name, it offers prefix names and automatically inserts the missing declaration after the last existing `PREFIX`/`BASE`/`@prefix`/`@base` line (matching `@prefix` style when already used in Turtle/TriG, and `PREFIX` otherwise). Completion is suppressed inside IRIs, strings, comments, and variables (`?` or `$`).

Known prefixes merge three sources, with later sources overriding earlier ones:

1. Sparkles' built-in well-known prefixes (such as `rdf`, `xsd`, `schema`, `prov`, and `mem`).
2. A remote dataset's active prefixes from `GET /$/prefixes/{dataset}`, when configured under `[lsp]`.
3. Explicit entries in the `[prefixes]` table of `.sparklesfmt.toml`.

Both tables live in the nearest `.sparklesfmt.toml` alongside formatter and `[lint]` settings (`sparkles fmt` and `sparkles lint` validate both tables and otherwise ignore them):

```toml
[prefixes]
kclj = "https://kclj.io/sparkles/"
memory = "https://kclj.io/sparkles/memory/"

[lsp]
server = "https://sparkles.example.org"
dataset = "slurp"
```

Prefix names must be empty (`""`) or start with a letter followed by letters, digits, `_`, `-`, or `.` without a trailing `.`, and all IRIs must be absolute. Invalid entries report a `config` diagnostic in the LSP and an error in `sparkles fmt`.

When `[lsp]` sets `server` and `dataset`, the language server fetches prefixes once per server/dataset pair in the background on document open without blocking completion, authenticating via `SPARKLES_TOKEN` or credentials saved by `sparkles auth login`. Plain `http` is refused for non-loopback hosts, failed fetches log to stderr while continuing with local sources, and without `[lsp]` the server never makes network calls or reads `SPARKLES_SERVER`. Builds without the `auth` feature log that `[lsp]` is ignored.

Because `.sparklesfmt.toml` lookup walks upward and stops at the first file without merging, `~/.sparklesfmt.toml` serves as a user-wide fallback, while a project-level `.sparklesfmt.toml` overrides it completely and must repeat any desired `[prefixes]` and `[lsp]` tables.

### Neovim (0.11 or later)

```lua
vim.lsp.config("sparkles", {
  cmd = { "sparkles", "lsp" },
  filetypes = { "sparql", "turtle", "trig", "ntriples", "nquads", "jsonld", "shaclc" },
  root_markers = { ".sparklesfmt.toml", "sparklesfmt.toml", ".git" },
})
vim.lsp.enable("sparkles")

-- Register extensions Neovim may not recognize and separate JSON-LD from plain JSON
vim.filetype.add({
  extension = {
    ru = "sparql", trig = "trig", nt = "ntriples", nq = "nquads", jsonld = "jsonld", shaclc = "shaclc",
  },
})
vim.treesitter.language.register("json", "jsonld")
```

Format buffers with `vim.lsp.buf.format()`. If you also configure `conform.nvim` below, `lsp_format = "fallback"` delegates only unconfigured filetypes to the language server.

### Helix

Add the language server and one `[[language]]` table per language to `languages.toml` (Helix merges keys into built-in definitions):

```toml
[language-server.sparkles]
command = "sparkles"
args = ["lsp"]

[[language]]
name = "sparql"
scope = "source.sparql"
file-types = ["rq", "ru", "sparql"]
language-servers = ["sparkles"]
auto-format = true

[[language]]
name = "turtle"
scope = "source.turtle"
file-types = ["ttl"]
language-servers = ["sparkles"]
auto-format = true
```

Run `:format` to format on demand, or keep `auto-format = true` to format on save.

## Formatter commands

Formatter plugins pipe buffer text through `sparkles fmt --stdin-filepath PATH`. The path is never opened; it selects the language by extension and locates the nearest `.sparklesfmt.toml`. Paths matching `.sparklesfmtignore` in the working directory pass through unchanged. On syntax errors, `sparkles fmt` writes nothing to stdout, prints `path:LINE:COL: error: …` to stderr, and exits with status `2` so the buffer remains intact.

### conform.nvim

```lua
require("conform").setup({
  formatters = {
    sparkles = {
      command = "sparkles",
      args = { "fmt", "--stdin-filepath", "$FILENAME" },
      stdin = true,
    },
  },
  formatters_by_ft = {
    sparql = { "sparkles" },
    turtle = { "sparkles" },
    trig = { "sparkles" },
    ntriples = { "sparkles" },
    nquads = { "sparkles" },
    jsonld = { "sparkles" },
    shaclc = { "sparkles" },
  },
  format_on_save = { timeout_ms = 2000, lsp_format = "fallback" },
})
```

### Helix (formatter command instead of the server)

```toml
[[language]]
name = "sparql"
formatter = { command = "sparkles", args = ["fmt", "--language", "sparql"] }
auto-format = true
```

Without a filename, `sparkles fmt` searches upward from its working directory for `.sparklesfmt.toml`, whereas `sparkles lsp` resolves the config file nearest to each open file.

### Emacs (apheleia)

```elisp
(with-eval-after-load 'apheleia
  (setf (alist-get 'sparkles apheleia-formatters)
        '("sparkles" "fmt" "--stdin-filepath" filepath))
  (dolist (mode '(sparql-mode ttl-mode))
    (setf (alist-get mode apheleia-mode-alist) 'sparkles)))
```

With `apheleia-mode` or `apheleia-global-mode` active, Emacs formats on save while preserving point.

### VS Code ("Run on Save")

There is no dedicated VS Code extension for Sparkles, though any generic LSP client extension can launch `sparkles lsp`. For command-based formatting, install the [Run on Save](https://marketplace.visualstudio.com/items?itemName=emeraldwalk.RunOnSave) extension (`emeraldwalk.runonsave`) and add the following to `settings.json`:

```json
{
  "emeraldwalk.runonsave": {
    "commands": [
      {
        "match": "\\.(rq|ru|sparql|ttl|trig|nt|nq|jsonld|shaclc)$",
        "cmd": "sparkles fmt --write \"${file}\""
      }
    ]
  }
}
```

Because `--write` rewrites a file only when formatting changes it, saving an already formatted file avoids unnecessary reloads.

## Related guides

See [Formatter and editor support](querying/README.md#formatter-and-editor-support) for the full list of supported formats and lint rules, and [Formatting](USAGE.md#formatting) and [Linting](USAGE.md#linting) for `.sparklesfmt.toml` options.
