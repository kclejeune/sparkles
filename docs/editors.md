# Editor integration

Two ways to format SPARQL (and the other languages `sparkles fmt` formats) from an
editor:

- **As a formatter command.** Formatter plugins pipe the buffer through
  `sparkles fmt --stdin-filepath PATH`, which reads stdin and prints the formatted text.
  The path is never opened. It only names the buffer: the language comes from its
  extension, the options from the `.sparklesfmt.toml` nearest to it, and a path matched
  by the working directory's `.sparklesfmtignore` comes back unchanged. On a syntax error
  it prints nothing on stdout, writes a `path:LINE:COL: error: …` line to stderr and exits
  with status 2, so the buffer is left as it is.
- **As a language server.** `sparkles lsp` speaks the Language Server Protocol over
  stdin and stdout. It formats documents (`textDocument/formatting`, and
  `textDocument/rangeFormatting`, which formats the whole document) and publishes syntax
  errors and the formatter's warnings as diagnostics while you type.

Both need the `sparkles` binary on the `PATH` (built with the `fmt` feature, on by
default).

## The language server

`sparkles lsp` (`--stdio` is accepted and changes nothing: stdio is the only transport)
offers:

- **Formatting** as one edit covering only the part of the document that changes, so
  undo, folds and marks elsewhere survive. A document that is already formatted gets no
  edit. Range formatting formats the whole document and returns the same edit, because
  a formatted range depends on what lies around it.
- **Diagnostics** on open and on every change, with source `sparkles fmt` and the kind
  as their code:
  - a syntax error (`syntax`) is an error at its position;
  - output the formatter refuses (`unsafe-format`, `unstable-format`: its safety checks
    failed, please report it) and a broken config file (`config`) are warnings;
  - the formatter's warnings about the document appear at their own position:
    `comment-moved` (a comment that sat where no element starts or ends, printed before
    the enclosing element) as a warning, and `undeclared-prefix` (a prefix used but not
    declared in the document) and any other kind as information. A warning without a
    position, such as `option-not-implemented` for a config key this build does not act
    on yet, appears at the start of the document.

  Diagnostics are cleared when the problem is fixed and when the document is closed. A
  document with a syntax error is not formatted; formatting it returns no edit rather
  than an error, since its diagnostic says why. Warnings do not stop formatting.
- **Options from `.sparklesfmt.toml` only**: the file nearest to the document, found
  the way `sparkles fmt` finds it, and read again on every request, so an edit to it
  applies at once. Documents that are not files (an unsaved buffer) use the defaults.
  There are no editor-side settings, so the editor, the command line and CI always agree.
- **The language** from the editor's language id (`sparql`, `turtle`, `ttl`, `trig`,
  `ntriples`, `nquads`, `jsonld`, `json-ld`), else from the file extension (as
  `sparkles fmt` reads it), else from the content. A language this build does not format
  yet is refused with "… formatting is not available yet" and gets no diagnostics.
- **Positions** in UTF-16 code units (the protocol's default), or in bytes when the
  client offers `utf-8` position encoding; lines end at `\n`, `\r\n` or `\r`. Formatted
  text always uses `\n` line endings.

It exits with status 0 after `shutdown` and `exit`, and 1 when the client exits or
disconnects without `shutdown`.

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

In `languages.toml` (one `[[language]]` table per language; where Helix already
defines the language, these keys are merged into its definition):

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

`:format` (or saving, with `auto-format`) asks the server.

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

Without a file name, `sparkles fmt` looks for `.sparklesfmt.toml` from its working
directory up; the language server finds the one nearest to each file.

### Emacs (apheleia)

```elisp
(with-eval-after-load 'apheleia
  (setf (alist-get 'sparkles apheleia-formatters)
        '("sparkles" "fmt" "--stdin-filepath" filepath))
  (dolist (mode '(sparql-mode ttl-mode))
    (setf (alist-get mode apheleia-mode-alist) 'sparkles)))
```

`apheleia-mode` (or `apheleia-global-mode`) then formats on save and keeps point where
it was.

### VS Code ("Run on Save")

VS Code has no generic command formatter, so this rewrites the file after each save with
the [Run on Save](https://marketplace.visualstudio.com/items?itemName=emeraldwalk.RunOnSave)
extension (`emeraldwalk.runonsave`); the editor reloads it. In `settings.json`:

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
`sparkles lsp` instead; there is no Sparkles extension.
