"""Hand-rolled Markdown → HTML for ``memd-wiki serve``.

P1 scope: render only the markdown dialect the compiler emits
(``tools/wiki/compiled_wiki/render.py``) plus the minimal superset
needed by LLM-authored concept-page bodies. Explicitly in scope:

- YAML frontmatter fenced by leading ``---`` pair
- ATX headings ``#`` … ``######``
- Unordered lists with ``- `` prefix and 2-space-indent continuation
  lines (no sub-bullet nesting — the deterministic compiler never
  emits them; LLM content that tries to nest falls back to flat
  rendering without breaking safety)
- Fenced code blocks with optional language tag
- Horizontal rules (standalone ``---`` at block start)
- Inline code backticks, ``[text](url)`` links, ``*italic*``,
  ``**bold**``

Everything else (tables, blockquotes, images, reference-style links,
HTML passthrough) is treated as plain paragraph text so it still
renders safely and readably. Zero third-party dependencies.

The renderer is pure string-in / string-out so callers (including
golden-byte tests) never need to bind a socket.
"""

from __future__ import annotations

import html
import re
from typing import Callable, Iterable, List, Optional, Tuple

LinkRewriter = Callable[[str], str]

_HEADING_RE = re.compile(r"^(#{1,6})\s+(.*)$")
_FENCE_RE = re.compile(r"^```(.*)$")
_INLINE_CODE_RE = re.compile(r"`([^`]*)`")
_LINK_RE = re.compile(r"\[([^\]]+)\]\(([^)]+)\)")
_BOLD_RE = re.compile(r"\*\*(.+?)\*\*")
_EMPH_RE = re.compile(r"\*([^\s*][^*]*?)\*")

# XSS guard: allow only benign schemes plus scheme-less (relative) URLs.
# ``javascript:``/``data:``/``vbscript:``/``file:`` are rejected and the
# link is rendered as literal markdown (``[label](url)`` escaped) so a
# reader still sees what was filtered instead of a silently dropped link.
_SAFE_URL_SCHEMES = frozenset({"http", "https", "mailto", "ftp", "ftps"})
_SCHEME_RE = re.compile(r"^([A-Za-z][A-Za-z0-9+.\-]*):")


def _is_safe_url(url: str) -> bool:
    """Return True if ``url`` uses a safe scheme or is scheme-less.

    Relative URLs (``foo.md``, ``/abs/path``, ``../sibling/``, ``#frag``)
    have no scheme and are always considered safe. A URL with a scheme
    must be in ``_SAFE_URL_SCHEMES``.
    """
    m = _SCHEME_RE.match(url)
    if m is None:
        return True
    return m.group(1).lower() in _SAFE_URL_SCHEMES

_PAGE_TEMPLATE = """<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>{title}</title>
<style>
body {{
  max-width: 960px;
  margin: 2em auto;
  padding: 0 1em;
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif;
  line-height: 1.5;
  color: #24292f;
}}
a {{ color: #0969da; }}
code {{
  background: #f6f8fa;
  padding: 0.15em 0.3em;
  border-radius: 3px;
  font-family: ui-monospace, "SFMono-Regular", Menlo, monospace;
  font-size: 0.95em;
}}
pre {{
  background: #f6f8fa;
  padding: 1em;
  border-radius: 6px;
  overflow-x: auto;
}}
pre code {{ background: transparent; padding: 0; }}
h1, h2, h3 {{ border-bottom: 1px solid #d0d7de; padding-bottom: 0.3em; }}
ul {{ padding-left: 2em; }}
hr {{ border: none; border-top: 1px solid #d0d7de; }}
.frontmatter {{
  border-left: 3px solid #d0d7de;
  color: #57606a;
  font-size: 0.9em;
}}
</style>
</head>
<body>
{body}
</body>
</html>
"""


def render_page(
    md: str,
    *,
    title: str = "memd-wiki",
    link_rewriter: Optional[LinkRewriter] = None,
) -> str:
    """Wrap the rendered body in a minimal self-contained HTML document."""
    body = markdown_to_html(md, link_rewriter=link_rewriter)
    return _PAGE_TEMPLATE.format(
        title=html.escape(title, quote=False),
        body=body,
    )


def markdown_to_html(
    md: str,
    *,
    link_rewriter: Optional[LinkRewriter] = None,
) -> str:
    """Render markdown to an HTML fragment (no ``<html>`` wrapper).

    ``link_rewriter`` is P3's hook: every ``[text](url)`` link has its
    ``url`` passed through the callable before the ``href`` attribute
    is emitted. In P1 the default is identity.
    """
    lines = md.splitlines()
    blocks = _parse_blocks(lines)
    return "\n".join(
        _render_block(block, link_rewriter=link_rewriter) for block in blocks
    )


Block = Tuple  # typing aid — each block is a tagged tuple, see below.


def _parse_blocks(lines: List[str]) -> List[Block]:
    """Split the line list into tagged blocks.

    Block tags:
    - ``("frontmatter", [line, ...])`` — YAML between ``---`` fences
      at document start
    - ``("heading", level, text)``
    - ``("code", lang, [line, ...])`` — fenced code block
    - ``("hr",)``
    - ``("list", [(text, [cont, ...]), ...])``
    - ``("para", [line, ...])``
    """
    blocks: List[Block] = []
    i = 0
    n = len(lines)

    if i < n and lines[i] == "---":
        j = i + 1
        while j < n and lines[j] != "---":
            j += 1
        if j < n:
            blocks.append(("frontmatter", list(lines[i + 1 : j])))
            i = j + 1

    while i < n:
        line = lines[i]
        if line == "":
            i += 1
            continue
        fence = _FENCE_RE.match(line)
        if fence is not None:
            lang = fence.group(1).strip()
            code_lines: List[str] = []
            i += 1
            while i < n and not lines[i].startswith("```"):
                code_lines.append(lines[i])
                i += 1
            if i < n:
                i += 1  # consume closing fence
            blocks.append(("code", lang, code_lines))
            continue
        heading = _HEADING_RE.match(line)
        if heading is not None:
            level = len(heading.group(1))
            blocks.append(("heading", level, heading.group(2).rstrip()))
            i += 1
            continue
        if line == "---":
            blocks.append(("hr",))
            i += 1
            continue
        if line.startswith("- "):
            items: List[Tuple[str, List[str]]] = []
            while i < n:
                cur = lines[i]
                if cur.startswith("- "):
                    items.append((cur[2:], []))
                elif cur.startswith("  ") and len(cur) >= 3 and cur[2] != " ":
                    if items:
                        items[-1][1].append(cur[2:])
                    else:
                        # Orphan continuation; treat as a standalone list item
                        items.append((cur[2:], []))
                elif cur == "":
                    i += 1
                    break
                else:
                    break
                i += 1
            blocks.append(("list", items))
            continue
        # Paragraph: consume up to the next block boundary.
        para: List[str] = [line]
        i += 1
        while i < n:
            nxt = lines[i]
            if (
                nxt == ""
                or nxt.startswith("#")
                or nxt.startswith("- ")
                or nxt.startswith("```")
                or nxt == "---"
            ):
                break
            para.append(nxt)
            i += 1
        blocks.append(("para", para))
    return blocks


def _render_block(
    block: Block,
    *,
    link_rewriter: Optional[LinkRewriter],
) -> str:
    kind = block[0]
    if kind == "frontmatter":
        lines: List[str] = block[1]
        escaped = "\n".join(html.escape(line) for line in lines)
        return f'<pre class="frontmatter">{escaped}\n</pre>'
    if kind == "heading":
        _, level, text = block
        inner = render_inline(text, link_rewriter=link_rewriter)
        return f"<h{level}>{inner}</h{level}>"
    if kind == "code":
        _, lang, code_lines = block
        escaped = html.escape("\n".join(code_lines))
        if lang:
            attr = f' class="language-{html.escape(lang, quote=True)}"'
        else:
            attr = ""
        return f"<pre><code{attr}>{escaped}\n</code></pre>"
    if kind == "hr":
        return "<hr/>"
    if kind == "list":
        items: List[Tuple[str, List[str]]] = block[1]
        rendered_items = []
        for text, conts in items:
            inner = render_inline(text, link_rewriter=link_rewriter)
            for cont in conts:
                inner += "<br>" + render_inline(cont, link_rewriter=link_rewriter)
            rendered_items.append(f"<li>{inner}</li>")
        return "<ul>\n" + "\n".join(rendered_items) + "\n</ul>"
    if kind == "para":
        para_lines: List[str] = block[1]
        inner = "<br>".join(
            render_inline(line, link_rewriter=link_rewriter) for line in para_lines
        )
        return f"<p>{inner}</p>"
    return ""


def render_inline(
    text: str,
    *,
    link_rewriter: Optional[LinkRewriter] = None,
) -> str:
    """Apply inline transforms to a single text span.

    Order: inline code (highest precedence; its inner text is never
    re-scanned) → links → bold → italic → plain text (HTML-escaped).
    Recursion into link labels, bold, and italic lets nested inline
    constructs render ("``*italic with [link](x)*``" works).
    """
    out: List[str] = []
    i = 0
    n = len(text)
    while i < n:
        ch = text[i]
        if ch == "`":
            m = _INLINE_CODE_RE.match(text, i)
            if m is not None:
                out.append(f"<code>{html.escape(m.group(1))}</code>")
                i = m.end()
                continue
        if ch == "[":
            m = _LINK_RE.match(text, i)
            if m is not None:
                label = m.group(1)
                url = m.group(2)
                rewritten = link_rewriter(url) if link_rewriter is not None else url
                if _is_safe_url(rewritten):
                    inner = render_inline(label, link_rewriter=link_rewriter)
                    out.append(
                        f'<a href="{html.escape(rewritten, quote=True)}">{inner}</a>'
                    )
                else:
                    # Preserve the raw markdown text of the rejected link so
                    # a reader can audit what was filtered (unlike a silent
                    # drop). The inner content is HTML-escaped so the URL
                    # cannot break out of the text context.
                    out.append(html.escape(m.group(0)))
                i = m.end()
                continue
        if ch == "*" and text.startswith("**", i):
            m = _BOLD_RE.match(text, i)
            if m is not None:
                inner = render_inline(m.group(1), link_rewriter=link_rewriter)
                out.append(f"<strong>{inner}</strong>")
                i = m.end()
                continue
        if ch == "*":
            m = _EMPH_RE.match(text, i)
            if m is not None:
                inner = render_inline(m.group(1), link_rewriter=link_rewriter)
                out.append(f"<em>{inner}</em>")
                i = m.end()
                continue
        out.append(html.escape(ch))
        i += 1
    return "".join(out)
