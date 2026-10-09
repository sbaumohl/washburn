"""Markdown -> HTML for findings and reports.

Raw HTML in the source is escaped, not passed through (``html: False``).
Links with unsafe schemes such as javascript: are dropped by markdown-it's
built-in link validator.
"""

from __future__ import annotations

from markdown_it import MarkdownIt

_md = (
    MarkdownIt("commonmark", {"html": False, "linkify": False, "typographer": False})
    .enable("table")
    .enable("strikethrough")
)


def render_markdown(text: str) -> str:
    return _md.render(text)
