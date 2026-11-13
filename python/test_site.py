"""the same site build serves pull-request checks and pages deployment."""

import base64
import contextlib
import hashlib
import io
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import build_site
from build_site import add_mermaid, add_wordmark, export_agents, page_url, redirects, write_redirects

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


class Wordmark(Folder):
    def test_the_home_page_shows_the_wordmark_the_cli_prints(self):
        root, site = self.folder(), self.folder()
        self.write(root / "src/output.rs", 'const WORDMARK: &str = r"\n /\\_<\n|_ _|\n";\n')
        self.write(site / "index.html", f"<article>{build_site.WORDMARK}<h1>docs</h1></article>")
        add_wordmark(root, site)
        self.assertEqual((site / "index.html").read_text(encoding="utf-8"),
                         '<article><pre class="fl-wordmark" role="img" aria-label="fetchloom">'
                         ' /\\_&lt;\n|_ _|</pre><h1>docs</h1></article>')

    def test_a_home_page_without_its_place_is_refused(self):
        root, site = self.folder(), self.folder()
        self.write(root / "src/output.rs", 'const WORDMARK: &str = r"\nmark\n";\n')
        self.write(site / "index.html", "<h1>docs</h1>")
        with self.assertRaisesRegex(ValueError, "no place for the wordmark"):
            add_wordmark(root, site)


class Build(unittest.TestCase):
    def test_zensical_builds_the_site_before_the_steps_that_finish_it(self):
        steps = []
        names = ("color_transcripts", "add_wordmark", "add_mermaid", "write_redirects", "export_agents")
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


if __name__ == "__main__":
    unittest.main()
