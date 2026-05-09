"""Pygments lexer for the Cute programming language.

Cute (https://github.com/i2y/cute) is a general-purpose language
designed for the Qt 6 / KDE Frameworks ecosystem. This lexer
follows the canonical keyword set defined in
``extensions/vscode-cute/syntaxes/cute.tmLanguage.json``.

Install (editable, while iterating)::

    pip install -e extensions/cute-pygments/

then ``pygmentize -l cute foo.cute`` and the ``cute`` Pygments
lexer alias become available. Markdown processors that delegate
to Pygments — Material for MkDocs / Zensical / Sphinx /
mkdocs-material's pymdownx.highlight — pick the lexer up
automatically through the ``pygments.lexers`` entry-point.
"""

from pygments.lexer import RegexLexer, bygroups, default, include, words
from pygments.token import (
    Comment,
    Keyword,
    Name,
    Number,
    Operator,
    Punctuation,
    String,
    Text,
    Whitespace,
)

__all__ = ["CuteLexer"]
__version__ = "0.1.0"


# Top-level + class-member declaration keywords (TextMate
# grammar's ``storage.type.cute``, with `store`, `suite`, `flags`
# added for v1.x).
_DECL_KEYWORDS = (
    "class",
    "struct",
    "arc",
    "enum",
    "flags",
    "error",
    "trait",
    "impl",
    "view",
    "widget",
    "style",
    "store",
    "suite",
    "test",
    "fn",
    "prop",
    "signal",
    "slot",
    "init",
    "deinit",
    "let",
    "var",
    "state",
)

# Visibility / storage modifiers (``storage.modifier.cute``).
# `value` is contextual (only meaningful after `extern` in
# `extern value Foo { ... }`) and `value` is a common identifier
# elsewhere — leave it out of the unconditional keyword set to
# avoid false positives. `extern` alone is enough to mark the
# surrounding form.
_MODIFIER_KEYWORDS = (
    "pub",
    "extern",
    "escaping",
    "consuming",
    "weak",
    "owned",
    "unowned",
    "readonly",
)

# Control flow + other reserved words (``keyword.control.cute``).
_CONTROL_KEYWORDS = (
    "if",
    "else",
    "case",
    "when",
    "match",
    "for",
    "while",
    "break",
    "continue",
    "return",
    "try",
    "emit",
    "batch",
    "async",
    "await",
)

# `use` / `import` etc. live in their own ``keyword.other.use``
# bucket; treat as keywords too.
_OTHER_KEYWORDS = ("use", "import")

# Property / signal / field modifiers used as contextual keywords
# *inside* a `prop` declaration (`prop x : T, bindable, default:
# V`). Pygments doesn't see context, but coloring them as keywords
# everywhere is harmless — they aren't common identifier names.
_CONTEXTUAL_KEYWORDS = (
    "bindable",
    "bind",
    "fresh",
    "notify",
    "default",
    "constant",
    "model",
    "of",
)

_CONSTANTS = ("true", "false", "nil")

# Built-in / stdlib type names. PascalCase identifiers are
# already coloured as ``Name.Class`` below, so this list is a
# safety net for the common ones in case the regex-based class
# detection misses one (it shouldn't, but the explicit list also
# helps in editors that style ``Keyword.Type`` differently).
_BUILTIN_TYPES = (
    "Int",
    "Float",
    "Bool",
    "String",
    "Void",
    "Self",
    "List",
    "Map",
    "Slice",
    "ModelList",
    "Future",
    "Result",
)


class CuteLexer(RegexLexer):
    """Pygments lexer for ``.cute`` source files."""

    name = "Cute"
    aliases = ["cute"]
    filenames = ["*.cute"]
    mimetypes = ["text/x-cute"]
    url = "https://github.com/i2y/cute"

    tokens = {
        "root": [
            include("whitespace"),
            include("comments"),
            include("strings"),
            include("symbols"),
            include("numbers"),
            # `~Copyable` opt-out modifier on class / struct / arc.
            (r"~Copyable\b", Keyword.Type),
            # `use qml "..."` form: highlight `qml` as a keyword
            # before falling through to the bare-`use` rule.
            (
                r"\b(use)\s+(qml)\b",
                bygroups(Keyword.Namespace, Keyword.Namespace),
            ),
            (words(_OTHER_KEYWORDS, suffix=r"\b"), Keyword.Namespace),
            (words(_DECL_KEYWORDS, suffix=r"\b"), Keyword.Declaration),
            (words(_MODIFIER_KEYWORDS, suffix=r"\b"), Keyword.Reserved),
            (words(_CONTROL_KEYWORDS, suffix=r"\b"), Keyword),
            (words(_CONTEXTUAL_KEYWORDS, suffix=r"\b"), Keyword.Pseudo),
            (words(_CONSTANTS, suffix=r"\b"), Keyword.Constant),
            (r"\b(self)\b", Name.Builtin.Pseudo),
            (words(_BUILTIN_TYPES, suffix=r"\b"), Keyword.Type),
            # PascalCase identifiers: types / view / widget / style
            # / class / arc / etc. (Cute house style: type names
            # PascalCase, value names camelCase).
            (r"[A-Z][A-Za-z0-9_]*", Name.Class),
            # `@name` attribute markers on `.qpi` fn decls
            # (`@lifted_bool_ok`). Bare `@x` in user code is a
            # parse error post-v0.x but the attribute form survives
            # in binding files.
            (r"@[A-Za-z_][A-Za-z0-9_]*", Name.Decorator),
            # Plain identifier (camelCase fields / methods / locals).
            (r"[a-z_][A-Za-z0-9_]*", Name),
            # Multi-char operators first so `..=` / `..` / `->` /
            # `=>` / `<=` / `>=` / `==` / `!=` etc. don't collapse
            # into single-char lexes.
            (r"\.\.=|\.\.|->|=>|::|\?\.|<=|>=|==|!=|&&|\|\||<<|>>", Operator),
            (r"[+\-*/%&|^~!<>=?]", Operator),
            (r"[(){}\[\]]", Punctuation),
            (r"[,;:.]", Punctuation),
        ],
        "whitespace": [
            (r"\s+", Whitespace),
        ],
        "comments": [
            # `#` to end of line. The leading `#{` interpolation
            # marker is handled inside string state below; at root
            # we will only ever see `#` followed by space / text.
            (r"#[^\n]*", Comment.Single),
        ],
        "strings": [
            (r'"', String, "string"),
        ],
        "string": [
            (r'[^"\\#]+', String),
            # Escape sequences: `\x..`, `\u{...}`, `\.`.
            (r"\\(x[0-9A-Fa-f]{2}|u\{[0-9A-Fa-f]+\}|.)", String.Escape),
            # `#{ expr [: format-spec] }` interpolation. The body
            # re-enters the root state so identifiers / keywords /
            # nested strings inside the interp get coloured.
            (r"#\{", String.Interpol, "interp"),
            # Bare `#` inside a string (not followed by `{`) is
            # just a literal character.
            (r"#", String),
            (r'"', String, "#pop"),
        ],
        "interp": [
            (r"\}", String.Interpol, "#pop"),
            include("root"),
        ],
        "symbols": [
            # `:fooBar` symbol literal — the leading `:` distinguishes
            # it from a type annotation `x : T` (which has whitespace
            # / non-identifier on the left). Use a negative
            # look-behind to avoid matching the colon inside `x:` or
            # `Foo::bar`.
            (
                r"(?<![A-Za-z0-9_:])(:)([A-Za-z_][A-Za-z0-9_]*)",
                bygroups(Punctuation, String.Symbol),
            ),
        ],
        "numbers": [
            (r"\b\d[\d_]*\.\d[\d_]*(?:[eE][+-]?\d+)?\b", Number.Float),
            (r"\b0[xX][0-9A-Fa-f][0-9A-Fa-f_]*\b", Number.Hex),
            (r"\b0[bB][01][01_]*\b", Number.Bin),
            (r"\b\d[\d_]*\b", Number.Integer),
        ],
    }
