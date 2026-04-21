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
    make_link_rewriter,
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

    def test_link_rejects_javascript_scheme(self) -> None:
        # LLM-authored concept bodies can attempt XSS via ``javascript:``.
        # The renderer falls back to rendering the raw markdown source as
        # HTML-escaped plain text, so the URL stays visible and inert.
        self.assertEqual(
            render_inline("[click](javascript:alert(1))"),
            "[click](javascript:alert(1))",
        )

    def test_link_rejects_data_scheme(self) -> None:
        self.assertEqual(
            render_inline("[x](data:text/html,<script>alert(1)</script>)"),
            "[x](data:text/html,&lt;script&gt;alert(1)&lt;/script&gt;)",
        )

    def test_link_rejects_scheme_case_insensitive(self) -> None:
        self.assertEqual(
            render_inline("[x](JaVaScRiPt:alert(1))"),
            "[x](JaVaScRiPt:alert(1))",
        )

    def test_link_rejected_when_rewriter_produces_unsafe_url(self) -> None:
        # XSS via a malicious rewriter still fails closed — the safety
        # check runs AFTER the rewriter, not before.
        bad_rewriter = lambda _u: "javascript:alert(1)"
        self.assertEqual(
            render_inline("[x](safe.md)", link_rewriter=bad_rewriter),
            "[x](safe.md)",
        )

    def test_link_allows_https_and_relative(self) -> None:
        self.assertEqual(
            render_inline("[a](https://example.com) and [b](../rel.md)"),
            '<a href="https://example.com">a</a> and '
            '<a href="../rel.md">b</a>',
        )

    def test_link_allows_fragment_and_mailto(self) -> None:
        self.assertEqual(
            render_inline("[a](#anchor) [b](mailto:x@y.com)"),
            '<a href="#anchor">a</a> <a href="mailto:x@y.com">b</a>',
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

    def test_hash_without_space_is_not_a_heading(self) -> None:
        # `#foo` is not a valid ATX heading (must be `# foo`).
        self.assertEqual(
            markdown_to_html("#foo"),
            "<p>#foo</p>",
        )

    def test_empty_input_produces_empty_html(self) -> None:
        self.assertEqual(markdown_to_html(""), "")

    def test_blockquote_silently_falls_back_to_paragraph(self) -> None:
        # `>` prefix is not in the supported dialect; pins the MVP
        # fallback so a future v3.1 upgrade is caught by tests.
        self.assertEqual(
            markdown_to_html("> quoted"),
            "<p>&gt; quoted</p>",
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


class LinkRewriterTests(unittest.TestCase):
    """Unit tests for ``make_link_rewriter`` (P3 rewriter behavior).

    Covers every link shape the deterministic compiler emits in
    ``compiled_wiki.render`` — ``render_index``, ``render_project_page``,
    ``render_task_page``, ``render_library_page``, ``render_log_page``,
    and the concept/entity page renderers — so a regression in the
    rewriter is caught before end-to-end HTTP round-trip.
    """

    def test_from_index_rewrites_lane_targets(self) -> None:
        rewrite = make_link_rewriter(Path("index.md"))
        self.assertEqual(rewrite("projects/memd.md"), "/projects/memd/")
        self.assertEqual(rewrite("tasks/019dadab.md"), "/tasks/019dadab/")
        self.assertEqual(
            rewrite("libraries/failures.md"), "/libraries/failures/"
        )
        self.assertEqual(rewrite("log.md"), "/log/")

    def test_from_index_rewrites_self_to_root(self) -> None:
        rewrite = make_link_rewriter(Path("index.md"))
        self.assertEqual(rewrite("index.md"), "/")

    def test_from_project_page_traverses_up(self) -> None:
        rewrite = make_link_rewriter(Path("projects/memd.md"))
        self.assertEqual(rewrite("../tasks/019dadab.md"), "/tasks/019dadab/")
        self.assertEqual(
            rewrite("../libraries/failures.md"), "/libraries/failures/"
        )
        self.assertEqual(rewrite("../log.md"), "/log/")

    def test_from_task_page_to_project_and_sibling(self) -> None:
        rewrite = make_link_rewriter(Path("tasks/019dadab.md"))
        self.assertEqual(rewrite("../projects/memd.md"), "/projects/memd/")
        self.assertEqual(
            rewrite("../tasks/other.md"), "/tasks/other/"
        )

    def test_from_concept_page_grounding_links(self) -> None:
        rewrite = make_link_rewriter(Path("concepts/abc-123.md"))
        # render_concept_grounding emits ``[task-id](../tasks/task-id.md)``.
        self.assertEqual(
            rewrite("../tasks/task-999.md"), "/tasks/task-999/"
        )

    def test_from_log_page_rewrites(self) -> None:
        rewrite = make_link_rewriter(Path("log.md"))
        self.assertEqual(rewrite("tasks/019dadab.md"), "/tasks/019dadab/")
        self.assertEqual(rewrite("projects/memd.md"), "/projects/memd/")

    def test_external_and_anchor_links_pass_through(self) -> None:
        rewrite = make_link_rewriter(Path("index.md"))
        for url in (
            "https://example.com/path",
            "http://127.0.0.1:8080/",
            "mailto:a@b.com",
            "#section-2",
            "/already-root/",
        ):
            self.assertEqual(rewrite(url), url, f"url={url}")

    def test_non_md_relative_link_pass_through(self) -> None:
        rewrite = make_link_rewriter(Path("index.md"))
        # Compiler does not emit these, but LLM-authored bodies might.
        self.assertEqual(rewrite("image.png"), "/image.png")
        self.assertEqual(rewrite("manifest.json"), "/manifest.json")

    def test_query_and_fragment_preserved(self) -> None:
        rewrite = make_link_rewriter(Path("index.md"))
        self.assertEqual(
            rewrite("tasks/019dadab.md#section-2"),
            "/tasks/019dadab/#section-2",
        )
        self.assertEqual(
            rewrite("tasks/019dadab.md?cachebust=1"),
            "/tasks/019dadab/?cachebust=1",
        )

    def test_escape_above_outdir_root_is_left_untouched(self) -> None:
        # From ``index.md``, ``../outside.md`` would normalize to ``..``
        # which can't be mapped to a route. Leave it unchanged so the
        # browser 404s rather than us emitting ``/../outside/``.
        rewrite = make_link_rewriter(Path("index.md"))
        self.assertEqual(rewrite("../outside.md"), "../outside.md")

    def test_empty_url_is_left_untouched(self) -> None:
        rewrite = make_link_rewriter(Path("index.md"))
        self.assertEqual(rewrite(""), "")

    def test_rewriter_integrates_with_render_page(self) -> None:
        # Pin the end-to-end contract: feeding the rewriter into
        # ``render_page`` produces an ``<a href="/tasks/abc/">`` anchor
        # for a compiler-style ``[x](tasks/abc.md)`` link.
        rewrite = make_link_rewriter(Path("index.md"))
        html_out = render_page(
            "- [see](tasks/abc.md)", title="t", link_rewriter=rewrite
        )
        self.assertIn('href="/tasks/abc/"', html_out)


if __name__ == "__main__":
    unittest.main()
