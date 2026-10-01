//! The ShExC lexer: lossless (whitespace and `#` comments are tokens, so the token texts
//! give back the input), with IRIs, prefixed names, blank-node labels, language tags,
//! the four string forms, numbers, regular expressions (`/…/flags`), semantic-action
//! code (`%iri{…%}`) and punctuation. A character no terminal starts with is an
//! `Unknown` token; the parser reports it.
