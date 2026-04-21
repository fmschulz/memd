"""Golden-byte tests for ``html_render`` (P1).

The renderer is deterministic string-in / string-out, so every test
pins exact expected output. Fixtures mirror the dialect emitted by
``compiled_wiki.render`` (verified in ``test_render.py``) plus the
minimal superset of constructs an LLM-authored concept body can use.
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from compiled_wiki.html_render import (  # noqa: E402
    markdown_to_html,
    render_inline,
    render_page,
)


class InlineTests(unittest.TestCase):
    def test_plain_text_is_html_escaped(self) -> None:
        self.assertEqual(
            render_inline("<script>alert(1)</script>"),
            "&lt;script&gt;alert(1)&lt;/script&gt;",
        )

    def test_inline_code_escapes_inner(self) -> None:
        self.assertEqual(
            render_inline("`<x>` and `y`"),
            "<code>&lt;x&gt;</code> and <code>y</code>",
        )

    def test_link_emits_anchor_with_escaped_href(self) -> None:
        self.assertEqual(
            render_inline("[home](https://example.com/?a=1&b=2)"),
            '<a href="https://example.com/?a=1&amp;b=2">home</a>',
        )

    def test_link_rewriter_is_applied(self) -> None:
        rewrite = lambda url: url.replace(".md", "/")
        self.assertEqual(
            render_inline("[goto](foo.md)", link_rewriter=rewrite),
            '<a href="foo/">goto</a>',
        )

    def test_italic_and_bold(self) -> None:
        self.assertEqual(
            render_inline("*(unresolved)*"),
            "<em>(unresolved)</em>",
        )
        self.assertEqual(
            render_inline("**loud** then *quiet*"),
            "<strong>loud</strong> then <em>quiet</em>",
        )

    def test_inline_code_contains_link_syntax_but_is_not_rescanned(self) -> None:
        self.assertEqual(
            render_inline("`[not-a-link](x)`"),
            "<code>[not-a-link](x)</code>",
        )

    def test_link_label_can_contain_inline_code(self) -> None:
        self.assertEqual(
            render_inline("[see `ref`](x)"),
            '<a href="x">see <code>ref</code></a>',
        )


class BlockTests(unittest.TestCase):
    def test_heading_levels(self) -> None:
        self.assertEqual(markdown_to_html("# H1"), "<h1>H1</h1>")
        self.assertEqual(markdown_to_html("### H3"), "<h3>H3</h3>")

    def test_paragraph_escapes_and_wraps(self) -> None:
        self.assertEqual(
            markdown_to_html("Hello <world>"),
            "<p>Hello &lt;world&gt;</p>",
        )

    def test_paragraph_line_breaks_become_br(self) -> None:
        self.assertEqual(
            markdown_to_html("one\ntwo"),
            "<p>one<br>two</p>",
        )

    def test_horizontal_rule(self) -> None:
        # HR only when it stands alone between blank lines (not adjacent to a
        # paragraph that would absorb the line as a continuation).
        self.assertEqual(
            markdown_to_html("text\n\n---\n\nmore"),
            "<p>text</p>\n<hr/>\n<p>more</p>",
        )

    def test_unordered_list_with_continuations(self) -> None:
        md = "- first item\n  continuation\n- second"
        self.assertEqual(
            markdown_to_html(md),
            "<ul>\n<li>first item<br>continuation</li>\n<li>second</li>\n</ul>",
        )

    def test_fenced_code_preserves_inner_and_escapes_html(self) -> None:
        md = "```rust\nfn main() { println!(\"<ok>\"); }\n```"
        self.assertEqual(
            markdown_to_html(md),
            "<pre><code class=\"language-rust\">fn main() { "
            "println!(&quot;&lt;ok&gt;&quot;); }\n</code></pre>",
        )

    def test_fenced_code_without_language(self) -> None:
        self.assertEqual(
            markdown_to_html("```\nplain\n```"),
            "<pre><code>plain\n</code></pre>",
        )

    def test_frontmatter_rendered_as_metadata_block(self) -> None:
        md = "---\nartifact_id: abc\ntitle: hi <x>\n---\n\n# Body\n"
        self.assertEqual(
            markdown_to_html(md),
            '<pre class="frontmatter">artifact_id: abc\n'
            "title: hi &lt;x&gt;\n</pre>\n<h1>Body</h1>",
        )

    def test_list_link_preserves_separator_dash(self) -> None:
        # Mirrors the compiler's "- [x](y) - meta" pattern from render.py.
        md = "- [tasks/task-1.md](tasks/task-1.md) - Goal name"
        self.assertEqual(
            markdown_to_html(md),
            '<ul>\n<li><a href="tasks/task-1.md">tasks/task-1.md</a> - '
            "Goal name</li>\n</ul>",
        )

    def test_trust_block_subheading_and_backticked_terms(self) -> None:
        md = (
            "## Trust\n\n"
            "- Trust tier: `compiled_digest_hint`\n"
            "- Requires verification: `True`\n"
            "- Reason: re-ground before trust\n"
        )
        self.assertEqual(
            markdown_to_html(md),
            "<h2>Trust</h2>\n<ul>\n"
            "<li>Trust tier: <code>compiled_digest_hint</code></li>\n"
            "<li>Requires verification: <code>True</code></li>\n"
            "<li>Reason: re-ground before trust</li>\n"
            "</ul>",
        )


class PageTemplateTests(unittest.TestCase):
    def test_render_page_wraps_body_with_title_and_style(self) -> None:
        html_out = render_page("# Hi", title="index.md")
        self.assertIn("<title>index.md</title>", html_out)
        self.assertIn("<h1>Hi</h1>", html_out)
        self.assertIn("<style>", html_out)
        self.assertTrue(html_out.startswith("<!DOCTYPE html>"))
        self.assertTrue(html_out.rstrip().endswith("</html>"))

    def test_render_page_escapes_title(self) -> None:
        html_out = render_page("text", title="<bad>")
        self.assertIn("<title>&lt;bad&gt;</title>", html_out)

    def test_render_page_link_rewriter_applied(self) -> None:
        html_out = render_page(
            "- [a](foo.md)",
            title="t",
            link_rewriter=lambda u: "/rewritten/" + u,
        )
        self.assertIn('href="/rewritten/foo.md"', html_out)


if __name__ == "__main__":
    unittest.main()
