"""the same site build serves pull-request checks and pages deployment."""

import base64
import contextlib
import hashlib
import io
from pathlib import Path
import re
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import build_site
from build_site import add_mermaid, add_pin, export_agents, page_url, redirects, write_redirects
import check_site

PAGES = "https://nikhil-yn-aix.github.io/fetchloom/"


class Folder(unittest.TestCase):
    def folder(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        return Path(folder.name)

    def write(self, path, text=""):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
        return path


class Addresses(unittest.TestCase):
    def test_a_page_is_a_folder_and_a_readme_its_folders_page(self):
        for page, url in [("README.md", ""), ("user/README.md", "user/"), ("user/commands/add.md", "user/commands/add/"),
                          ("dev/style.md", "dev/style/"), ("user/READMEs.md", "user/READMEs/")]:
            with self.subTest(page=page):
                self.assertEqual(page_url(page), url)


class Exports(Folder):
    def checkout(self, root, summary):
        self.write(root / "skills/fetchloom/SKILL.md", "skill")
        self.write(root / "docs/user/one.md", "\nfirst\n\n")
        self.write(root / "docs/user/two.md", "second")
        self.write(root / "docs/SUMMARY.md", summary)
        self.write(root / "llms.txt",
                   "[one](docs/user/one.md#heading) [two](docs/user/two.md) [guide](docs/user/README.md) "
                   "[skill](skills/fetchloom/SKILL.md) [code](src/lib.rs) [chart](docs/assets/bench/chart.svg) "
                   "[site](https://example.org/a) [mail](mailto:a@b.c)\n\n")
        site = root / "site"
        site.mkdir()
        return site

    def test_exports_link_the_sites_pages_and_follow_summary_order(self):
        root = self.folder()
        site = self.checkout(root, "- [two](user/two.md)\n- [one](user/one.md)")
        printed = io.StringIO()
        with contextlib.redirect_stdout(printed):
            export_agents(root, site)
        self.assertEqual(printed.getvalue(), "llms-full.txt has 2 pages\n")
        blob = "https://github.com/nikhil-yn-aix/fetchloom/blob/main/"
        self.assertEqual((site / "llms.txt").read_text(encoding="utf-8"),
                         "[one](user/one/#heading) [two](user/two/) [guide](user/) [skill](skills/fetchloom/SKILL.md) "
                         f"[code]({blob}src/lib.rs) [chart]({blob}docs/assets/bench/chart.svg) "
                         "[site](https://example.org/a) [mail](mailto:a@b.c)\n\n"
                         "## optional\n\n- [full user guide](llms-full.txt): every user guide page in one file\n")
        self.assertEqual((site / "llms-full.txt").read_text(encoding="utf-8"),
                         "# fetchloom user guide\n\n"
                         f"every page of the fetchloom user guide in reading order, from {PAGES}user/\n\n"
                         f"---\n\nsource: {PAGES}user/two/\n\nsecond\n\n"
                         f"---\n\nsource: {PAGES}user/one/\n\nfirst\n")
        self.assertEqual((site / "skills/fetchloom/SKILL.md").read_text(encoding="utf-8"), "skill")

    def test_a_summary_without_user_pages_is_refused(self):
        root = self.folder()
        site = self.checkout(root, "- [developer guide](dev/README.md)")
        with self.assertRaisesRegex(ValueError, "docs/SUMMARY.md lists no user guide pages"):
            export_agents(root, site)
        self.assertFalse((site / "llms-full.txt").exists())


class Redirects(Folder):
    def site(self, *urls):
        site = self.folder()
        for url in urls:
            self.write(site / url / "index.html", "page")
        return site

    def moved(self, site, rows):
        with contextlib.redirect_stdout(io.StringIO()):
            write_redirects(site, rows)

    def test_an_old_address_sends_a_visitor_on_with_their_fragment(self):
        site = self.site("user/commands/add/", "")
        self.moved(site, [("user/commands/add.html", "user/commands/add.md"), ("print.html", "README.md")])
        stub = (site / "user/commands/add.html").read_text(encoding="utf-8")
        self.assertIn(f'<link rel="canonical" href="{PAGES}user/commands/add/">', stub)
        self.assertIn('<script>location.replace("add/" + location.hash)</script>', stub)
        self.assertIn('<meta http-equiv="refresh" content="0; url=add/">', stub)
        self.assertIn('<a href="add/">', stub)
        self.assertIn('location.replace("./" + location.hash)', (site / "print.html").read_text(encoding="utf-8"))

    def test_a_readme_address_goes_to_its_folder(self):
        site = self.site("user/commands/", "dev/")
        self.moved(site, [("user/commands/README.html", "user/commands/README.md"),
                          ("user/commands/old.html", "dev/README.md")])
        self.assertIn('url=./"', (site / "user/commands/README.html").read_text(encoding="utf-8"))
        self.assertIn('url=../../dev/"', (site / "user/commands/old.html").read_text(encoding="utf-8"))

    def test_a_redirect_to_a_page_the_site_lacks_is_refused(self):
        site = self.site("user/")
        with self.assertRaisesRegex(ValueError, "user/gone.html redirects to user/gone.md, which the site does not have"):
            self.moved(site, [("user/gone.html", "user/gone.md")])
        self.assertFalse((site / "user/gone.html").exists())

    def test_a_redirect_never_replaces_a_page(self):
        site = self.site("user/")
        self.write(site / "404.html", "not found")
        with self.assertRaisesRegex(ValueError, "404.html is a page of the site"):
            self.moved(site, [("404.html", "user/README.md")])
        self.assertEqual((site / "404.html").read_text(encoding="utf-8"), "not found")

    def test_the_table_reads_rows_and_skips_notes(self):
        root = self.folder()
        self.write(root / "docs-site/redirects.txt", "# a note\n\nprint.html    README.md\nuser/a.html   user/a.md\n")
        self.assertEqual(redirects(root), [("print.html", "README.md"), ("user/a.html", "user/a.md")])


class Mermaid(Folder):
    def package(self, script=b"mermaid"):
        data = io.BytesIO()
        with tarfile.open(fileobj=data, mode="w:gz") as archive:
            for name, body in (("package/dist/mermaid.tiny.js", script), ("package/LICENSE", b"MIT")):
                member = tarfile.TarInfo(name)
                member.size = len(body)
                archive.addfile(member, io.BytesIO(body))
        return data.getvalue()

    def pinned(self, data):
        return ("https://registry.npmjs.org/@mermaid-js/tiny/-/tiny-0.tgz",
                "sha512-" + base64.b64encode(hashlib.sha512(data).digest()).decode())

    def test_the_script_and_its_licence_come_from_the_pinned_package(self):
        site, cache = self.folder(), self.folder()
        (site / "assets/javascripts").mkdir(parents=True)
        data = self.package()
        (cache / "tiny-0.tgz").write_bytes(data)
        with patch.object(build_site, "MERMAID", self.pinned(data)), patch.object(build_site.urllib.request, "urlopen") as fetch:
            add_mermaid(site, cache)
        fetch.assert_not_called()
        self.assertEqual((site / "assets/javascripts/mermaid.tiny.js").read_bytes(), b"mermaid")
        self.assertEqual((site / "assets/javascripts/mermaid.LICENSE.txt").read_bytes(), b"MIT")

    def test_a_download_unlike_the_pin_is_refused(self):
        site, cache = self.folder(), self.folder()
        (site / "assets/javascripts").mkdir(parents=True)
        pinned = self.pinned(self.package())
        response = io.BytesIO(self.package(b"something else"))
        with patch.object(build_site, "MERMAID", pinned), patch.object(build_site.urllib.request, "urlopen", return_value=response):
            with self.assertRaisesRegex(ValueError, "does not have the integrity pinned for it"):
                add_mermaid(site, cache)
        self.assertEqual(list(cache.iterdir()), [])
        self.assertFalse((site / "assets/javascripts/mermaid.tiny.js").exists())


class Pin(Folder):
    def source(self, lines='("\u25cf", "o"), ("\u2502", "|"), ("\u2575", "\'")'):
        """a repository whose src/output.rs draws a pin of ``lines``, with the site's two accents."""
        root = self.folder()
        self.write(root / "src/output.rs", f"const PIN: [(&str, &str); 3] = [{lines}];\n")
        self.write(root / "docs-site/theme/assets/stylesheets/fetchloom.css",
                   '[data-md-color-scheme="default"] {\n  --fl-accent: #1b6a54;\n}\n'
                   '[data-md-color-scheme="slate"] {\n  --fl-accent: #79c9ae;\n}\n')
        return root

    def test_the_site_and_its_favicon_draw_the_pin_the_cli_draws(self):
        root, site = self.source(), self.folder()
        add_pin(root, site)
        shaft = "M4.2 10h1.6v10h-1.6zM4.2 20h1.6v20h-1.6zM4.2 40h1.6L5 50z"
        self.assertEqual((site / "assets/images/pin.svg").read_text(encoding="utf-8"),
                         '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 5.5 10 44.5">'
                         f'<path d="M0.5 10a4.5 4.5 0 1 1 9 0a4.5 4.5 0 1 1 -9 0z{shaft}"/></svg>\n')
        self.assertEqual((site / "assets/images/favicon.svg").read_text(encoding="utf-8"),
                         '<svg xmlns="http://www.w3.org/2000/svg" viewBox="-19.5 1 49 49">'
                         '<style>path{fill:#1b6a54}@media (prefers-color-scheme:dark){path{fill:#79c9ae}}</style>'
                         '<path d="M-4 10a9 9 0 1 1 18 0a9 9 0 1 1 -18 0zM2.8 10h4.4v10h-4.4z'
                         'M2.8 20h4.4v20h-4.4zM2.8 40h4.4L5 50z"/></svg>\n')

    def test_a_longer_shaft_in_the_terminal_is_a_longer_pin_on_the_site(self):
        root, site = self.source('("\u25cf", "o"), ("\u2502", "|"), ("\u2502", "|"), ("\u2575", "\'")'), self.folder()
        add_pin(root, site)
        self.assertIn('viewBox="0 5.5 10 64.5"', (site / "assets/images/pin.svg").read_text(encoding="utf-8"))

    def test_a_source_without_a_pin_is_refused(self):
        for lines in ('("\u2502", "|"), ("\u25cf", "o")', '("\u25cf", "o"), ("#", "#")'):
            with self.subTest(lines=lines), self.assertRaisesRegex(ValueError, "src/output.rs has no PIN"):
                add_pin(self.source(lines), self.folder())

    def test_the_repository_draws_a_pin_the_site_can_draw(self):
        self.assertEqual(build_site.pin(build_site.ROOT)[0], "\u25cf")


def luminance(color):
    """the relative luminance WCAG 2 gives a #rrggbb color."""
    channels = [int(color[i:i + 2], 16) / 255 for i in (1, 3, 5)]
    red, green, blue = [c / 12.92 if c <= .03928 else ((c + .055) / 1.055) ** 2.4 for c in channels]
    return .2126 * red + .7152 * green + .0722 * blue


def schemes():
    """the site's light colors and its dark ones, by name, from its stylesheet."""
    css = (build_site.ROOT / build_site.STYLESHEET).read_text(encoding="utf-8")
    found = {}
    for scheme in ("default", "slate"):
        block = re.search(r'\[data-md-color-scheme="' + scheme + r'"\] \{([^}]*)\}', css)[1]
        found[scheme] = dict(re.findall(r"--fl-(\w+): (#[0-9a-f]{6});", block))
    return found


class Colors(unittest.TestCase):
    def test_every_text_color_of_the_site_reads_on_its_paper(self):
        """WCAG AA for small text, 4.5 to 1, in the light colors and the dark ones, on the paper, on
        the surface of labels, code and tables, and on the band of a table's head."""
        for scheme, colors in schemes().items():
            grounds = [("paper", name) for name in ("ink", "muted", "accent", "done", "warn", "fail")]
            grounds += [("surface", name) for name in ("ink", "muted", "accent", "done", "warn", "fail")]
            grounds += [("band", "ink")]
            for ground, name in grounds:
                with self.subTest(scheme=scheme, ground=ground, name=name):
                    lighter, darker = sorted((luminance(colors[name]), luminance(colors[ground])), reverse=True)
                    self.assertGreaterEqual((lighter + .05) / (darker + .05), 4.5)

    def test_the_cli_themes_take_the_site_colors(self):
        """src/output/style.rs's light and dark themes are the site's colors: work and accent its
        accent, done, warn and fail its own."""
        style = (build_site.ROOT / "src/output/style.rs").read_text(encoding="utf-8")
        for theme, scheme in (("Light", "default"), ("Dark", "slate")):
            block = re.search(r"Theme::" + theme + r" => Palette \{(.*?)\}", style, re.S)[1]
            tones = {name: f"#{red}{green}{blue}" for name, red, green, blue
                     in re.findall(r"(\w+): rgb\(0x(\w\w), 0x(\w\w), 0x(\w\w)\)", block)}
            colors = schemes()[scheme]
            self.assertEqual(tones, {"done": colors["done"], "work": colors["accent"], "warn": colors["warn"],
                                     "fail": colors["fail"], "accent": colors["accent"]}, theme)


class Build(unittest.TestCase):
    def test_zensical_builds_the_site_before_the_steps_that_finish_it(self):
        steps = []
        names = ("color_transcripts", "add_pin", "add_mermaid", "write_redirects", "export_agents")
        with patch.object(build_site.subprocess, "run", side_effect=lambda *a, **k: steps.append("zensical")) as run, \
             patch.object(build_site.shutil, "rmtree", side_effect=lambda *a, **k: steps.append("clean")), \
             patch.object(build_site, "redirects", return_value=[]), \
             contextlib.ExitStack() as stack:
            for name in names:
                stack.enter_context(patch.object(build_site, name, side_effect=lambda *a, name=name: steps.append(name)))
            build_site.main()
        self.assertEqual(steps, ["clean", "zensical", *names])
        run.assert_called_once_with([sys.executable, "-m", "zensical", "build", "--clean", "--strict"],
                                    cwd=build_site.ROOT, check=True)


class BuiltSite(Folder):
    """the checks python/check_site.py runs on a built site, on small sites."""

    def page(self, site, path, body="", head=""):
        return self.write(site / path, f"<html><head>{head}</head><body>{body}</body></html>")

    def moved(self, site, address, target):
        self.write(site / address, f'<link rel="canonical" href="x"><meta http-equiv="refresh" content="0; url={target}">')

    def test_the_addresses_fetchloom_prints_are_found_in_its_sources(self):
        root = self.folder()
        self.write(root / "docs/user/commands/add.md")
        self.write(root / "docs/user/commands/sync.md")
        self.write(root / "docs/user/commands/README.md")
        self.write(root / "src/cli/args.rs", 'const SITE: &str = "https://nikhil-yn-aix.github.io/fetchloom";\n'
                                             'format!("{SITE}/user/commands/README.html"); format!("{SITE}/user/commands/{name}.html");')
        self.write(root / "src/source/gitlab.rs", 'const SITE: &str = "https://gitlab.com";\nformat!("{SITE}/api/v4/projects")')
        self.write(root / "src/place.rs", '"see https://nikhil-yn-aix.github.io/fetchloom/user/hpc.html".to_owned()')
        self.write(root / "tests/cli/snapshots/help.snap",
                   "docs: https://nikhil-yn-aix.github.io/fetchloom/user/sources/buckets.html#private-s3-buckets.\n")
        self.assertEqual(check_site.printed(root), {
            "https://nikhil-yn-aix.github.io/fetchloom",
            "https://nikhil-yn-aix.github.io/fetchloom/user/commands/README.html",
            "https://nikhil-yn-aix.github.io/fetchloom/user/commands/add.html",
            "https://nikhil-yn-aix.github.io/fetchloom/user/commands/sync.html",
            "https://nikhil-yn-aix.github.io/fetchloom/user/hpc.html",
            "https://nikhil-yn-aix.github.io/fetchloom/user/sources/buckets.html#private-s3-buckets",
        })

    def test_an_address_reaches_its_page_and_heading_through_a_moved_page(self):
        site = self.folder()
        self.page(site, "index.html")
        self.page(site, "user/hpc/index.html", '<h2 id="scratch">scratch</h2>')
        self.moved(site, "user/hpc.html", "hpc/")
        for address in ("", "/", "/user/hpc/", "/user/hpc", "/user/hpc.html", "/user/hpc.html#scratch", "/user/hpc/#scratch"):
            with self.subTest(address=address):
                self.assertIsNone(check_site.unreached(site, check_site.SITE_URL + address))
        self.assertEqual(check_site.unreached(site, check_site.SITE_URL + "/user/gone.html"),
                         f"{check_site.SITE_URL}/user/gone.html reaches no page")
        self.assertEqual(check_site.unreached(site, check_site.SITE_URL + "/user/hpc.html#nodes"),
                         f"{check_site.SITE_URL}/user/hpc.html has no #nodes on user/hpc/index.html")

    def test_a_link_to_a_markdown_file_is_found(self):
        site = self.folder()
        self.page(site, "user/index.html", '<a href="../dev/style.md#prose">style</a><a href="add/">add</a>'
                                           '<a href="https://github.com/nikhil-yn-aix/fetchloom/blob/main/README.md">readme</a>')
        self.assertEqual(check_site.markdown_links(site), ["user/index.html links ../dev/style.md#prose"])

    def test_a_heading_without_its_github_id_is_found(self):
        root, site = self.folder(), self.folder()
        self.write(root / "docs/README.md", "# docs\n## the `sync` command\n")
        self.write(root / "docs/user/a.md", "# a\n## what's new?\n")
        self.page(site, "index.html", '<h1 id="docs">docs</h1><h2 id="the-sync-command">the sync command</h2>')
        self.page(site, "user/a/index.html", '<h1 id="a">a</h1><h2 id="whats_new">what\'s new?</h2>')
        self.assertEqual(check_site.github_ids(root, site), ["user/a.md has no id whats-new on user/a/index.html"])

    def test_anything_a_page_loads_from_another_host_is_found(self):
        site = self.folder()
        self.page(site, "index.html",
                  '<a href="https://example.org/">a link</a><img src="assets/a.png">'
                  '<script src="https://unpkg.com/mermaid@12/dist/mermaid.min.js"></script>',
                  '<link rel="canonical" href="https://nikhil-yn-aix.github.io/fetchloom/">'
                  '<link rel="stylesheet" href="//fonts.googleapis.com/css">')
        self.write(site / "assets/a.css", '@font-face{src:url("../fonts/a.woff2")}'
                                          'a{background:url(https://example.org/b.png)}@import "https://example.org/c.css";')
        self.assertEqual(check_site.external(site), [
            "assets/a.css loads https://example.org/b.png",
            "assets/a.css loads https://example.org/c.css",
            "index.html loads //fonts.googleapis.com/css",
            "index.html loads https://unpkg.com/mermaid@12/dist/mermaid.min.js",
        ])


if __name__ == "__main__":
    unittest.main()
