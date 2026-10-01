# Editor integration

An editor can format SPARQL, and the other languages `sparkles fmt` formats, in two ways:

- **As a formatter command.** Formatter plugins pipe the buffer through
  `sparkles fmt --stdin-filepath PATH`, which reads stdin and prints the formatted text.
  The path is never opened; it only names the buffer. The language comes from its
  extension and the options from the `.sparklesfmt.toml` nearest to it. A path that
  matches the working directory's `.sparklesfmtignore` comes back unchanged. On a syntax
  error, the command prints nothing on stdout, writes a `path:LINE:COL: error: …` line to
  stderr and exits with status 2, so the buffer is left as it is.
- **As a language server.** `sparkles lsp` speaks the Language Server Protocol over stdin
  and stdout. It formats documents and publishes syntax errors and the formatter's
  warnings as diagnostics while you type. It handles `textDocument/formatting` and
  `textDocument/rangeFormatting`, which formats the whole document.

Both need the `sparkles` binary on the `PATH`, built with the `fmt` feature (on by
default).

## The language server

`sparkles lsp` talks over stdio, its only transport. It accepts `--stdio`, which changes
nothing. The server provides:

- **Formatting.** The server returns one edit that covers only the part of the document
  that changes, so undo, folds and marks elsewhere survive. A document that is already
  formatted gets no edit. Range formatting formats the whole document and returns the
  same edit, because a formatted range depends on what lies around it.
- **Diagnostics.** The server publishes diagnostics when a document opens and on every
  change. Each has the source `sparkles fmt` and its kind as the code:
  - A syntax error (`syntax`) is an error at its position.
  - Output the formatter refuses (`unsafe-format`, `unstable-format`) is a warning. It
    means the formatter's safety checks failed; please report it. A broken config file
    (`config`) is also a warning.
  - The formatter's warnings about the document appear at their own position.
    `comment-moved` is a warning: a comment sat where no element starts or ends, and was
    printed before the enclosing element. `undeclared-prefix`, for a prefix used but not
    declared in the document, is information, as is any other kind. A warning without a
    position appears at the start of the document. An example is `option-not-implemented`,
    for a config key this build does not act on yet.

  Diagnostics are cleared when the problem is fixed and when the document is closed. A
  document with a syntax error is not formatted. Formatting it returns no edit rather
  than an error, because its diagnostic already says why. Warnings do not stop
  formatting.
- **Options.** Options come only from `.sparklesfmt.toml`. The server uses the file
  nearest to the document, found the way `sparkles fmt` finds it, and reads it again on
  every request, so an edit to it applies at once. Documents that are not files, such as
  an unsaved buffer, use the defaults. There are no editor-side settings, so the editor,
  the command line and CI always agree.
- **Language.** The server takes the language from the editor's language id (`sparql`,
  `turtle`, `ttl`, `trig`, `ntriples`, `nquads`, `jsonld`, `json-ld`). Failing that, it
  uses the file extension, as `sparkles fmt` reads it, and then the content. A language
  this build does not format yet is refused with "… formatting is not available yet" and
  gets no diagnostics.
- **Positions.** Positions are in UTF-16 code units, the protocol's default, or in bytes
  when the client offers the `utf-8` position encoding. Lines end at `\n`, `\r\n` or
  `\r`. Formatted text always uses `\n` line endings.

The server exits with status 0 after `shutdown` and `exit`, and with status 1 when the
client exits or disconnects without `shutdown`.

### Neovim (0.11 or later)

```lua
vim.lsp.config("sparkles", {
  cmd = { "sparkles", "lsp" },
  filetypes = { "sparql", "turtle", "trig", "ntriples", "nquads", "jsonld" },
  root_markers = { ".sparklesfmt.toml", "sparklesfmt.toml", ".git" },
})
vim.lsp.enable("sparkles")

-- extensions Neovim may not know, and JSON-LD apart from plain JSON
vim.filetype.add({
  extension = { ru = "sparql", trig = "trig", nt = "ntriples", nq = "nquads", jsonld = "jsonld" },
})
vim.treesitter.language.register("json", "jsonld")
```

Format with `vim.lsp.buf.format()`. With conform.nvim configured as below,
`lsp_format = "fallback"` uses the server only for filetypes conform has no formatter
for.

### Helix

Add this to `languages.toml`, with one `[[language]]` table per language. Where Helix
already defines a language, these keys are merged into its definition.

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

`:format` asks the server to format the buffer. With `auto-format`, saving does too.

## Formatter commands

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

Without a file name, `sparkles fmt` looks for `.sparklesfmt.toml` in its working
directory and then in each parent. The language server instead finds the one nearest to
each file.

### Emacs (apheleia)

```elisp
(with-eval-after-load 'apheleia
  (setf (alist-get 'sparkles apheleia-formatters)
        '("sparkles" "fmt" "--stdin-filepath" filepath))
  (dolist (mode '(sparql-mode ttl-mode))
    (setf (alist-get mode apheleia-mode-alist) 'sparkles)))
```

With `apheleia-mode` or `apheleia-global-mode` on, Emacs formats on save and keeps point
where it was.

### VS Code ("Run on Save")

VS Code has no generic command formatter. Instead, the
[Run on Save](https://marketplace.visualstudio.com/items?itemName=emeraldwalk.RunOnSave)
extension (`emeraldwalk.runonsave`) can rewrite the file after each save, and the editor
reloads it. Add this to `settings.json`:

```json
{
  "emeraldwalk.runonsave": {
    "commands": [
      {
        "match": "\\.(rq|ru|sparql|ttl|trig|nt|nq|jsonld)$",
        "cmd": "sparkles fmt --write \"${file}\""
      }
    ]
  }
}
```

`--write` replaces the file only when it changes, so saving a formatted file does not
reload it. Any VS Code extension that starts a language server for a file type can run
`sparkles lsp` instead. There is no Sparkles extension.
