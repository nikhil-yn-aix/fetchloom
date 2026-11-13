"""transcripts in the docs take the colors src/output.rs gives each line, on real runs the docs show."""

import unittest

from transcript import colored, is_transcript, render


class Lines(unittest.TestCase):
    def test_a_run_that_worked(self):
        # docs/user/commands/add.md
        run = (" Downloaded iris.csv · 32 B in [time]\n"
               "    Started a project in [sandbox]/work\n"
               "      Added iris, its files are in data/iris\n"
               "  hint: scripts can find it with `fetchloom path iris`\n")
        self.assertEqual(render(run), (
            ' <span class="fl-done">Downloaded</span> <span class="fl-bold">iris.csv</span>'
            '<span class="fl-dim"> · 32 B in [time]</span>\n'
            '    <span class="fl-done">Started</span> a project in [sandbox]/work\n'
            '      <span class="fl-done">Added</span> iris, its files are in data/iris\n'
            '  <span class="fl-dim">hint: scripts can find it with </span>'
            '<span class="fl-accent">`fetchloom path iris`</span>\n'))

    def test_a_warning_with_its_help_and_the_next_step(self):
        run = ('warning: a later run downloads from http://127.0.0.1:19450, a private address\n'
               '  help: put `allowed_origins = ["http://127.0.0.1:19450"]` in the settings file\n'
               '  next: commit data.toml, then runs `fetchloom sync`\n')
        self.assertEqual(render(run), (
            '<span class="fl-warn">warning:</span> a later run downloads from http://127.0.0.1:19450, '
            'a private address\n'
            '  <span class="fl-accent">help:</span> put '
            '<span class="fl-accent">`allowed_origins = ["http://127.0.0.1:19450"]`</span> in the settings file\n'
            '  <span class="fl-dim">next: commit data.toml, then runs </span>'
            '<span class="fl-accent">`fetchloom sync`</span>\n'))

    def test_an_error_after_the_command_typed(self):
        # docs/user/commands/drop.md, whose details are dim under the error, help or not
        run = ("$ fetchloom drop 'data/dot/*.wav'\n"
               "error: data/dot/*.wav names no file of dot\n"
               "  drop takes paths, not patterns, so * is part of the name it looks for\n"
               "  help: add \"*.wav\" to exclude by hand, then run `fetchloom sync`\n"
               "  127.0.0.1 refused access to http://127.0.0.1:[port]/denied.csv (403)\n"
               "    http://127.0.0.1:[port]/gone/b.csv\n")
        self.assertEqual(render(run), (
            '<span class="fl-dim">$</span> <span class="fl-typed">fetchloom drop \'data/dot/*.wav\'</span>\n'
            '<span class="fl-fail">error:</span> data/dot/*.wav names no file of dot\n'
            '  <span class="fl-dim">drop takes paths, not patterns, so * is part of the name it looks for</span>\n'
            '  <span class="fl-accent">help:</span> add "*.wav" to exclude by hand, then run '
            '<span class="fl-accent">`fetchloom sync`</span>\n'
            '  <span class="fl-dim">127.0.0.1 refused access to http://127.0.0.1:[port]/denied.csv (403)</span>\n'
            '  <span class="fl-dim">  http://127.0.0.1:[port]/gone/b.csv</span>\n'))

    def test_each_verb_takes_the_tone_its_function_gives_it(self):
        cases = [
            # step's verb_color
            ("Resolving zenodo:7654321", "work"),
            ("Downloading big.bin · 1.9 MiB", "work"),
            ("Missing data/iris/iris.csv", "warn"),
            ("Damaged data/iris/iris.csv · linked to the cache's copy", "fail"),
            ("Resolved s3://wds/shards/ · 1,024 files", "done"),
            ("Placed data/clean · 1 file linked to the cache", "done"),
            ("Synced small into data", "done"),
            # setup lines, bars and tickers draw their verb as work under way
            ("Locking the project · 3 s", "work"),
            ("Placing 20,000 files", "work"),
            # a difference mark or report finds
            ("Differs emb · 0 of 1 file match data.lock", "warn"),
            ("Unbuilt clean · its pinned result is not on this machine", "warn"),
            ("Found 2 differences from data.lock", "warn"),
            ("Found 12 datasets for \"iris\"", "done"),
            ("Replaced 1 damaged copy on the mirror", "warn"),
            ("Replaced other.csv", "done"),
            ("Removed wine · not in data.toml, so `fetchloom sync` takes it out of data.lock", "warn"),
            ("Removed 1 file from data/ that no dataset needs", "done"),
            ("Skipped tidy · it reads clean's result as data.lock pins it", "warn"),
            ("Skipped 6 files already there · 106 B", "done"),
            # a stop or cancel the person chose
            ("Cancelled signing in to hugging face", "warn"),
            ("Stopping (press ctrl-c again to quit at once)", "warn"),
            ("Finishing what it started (press ctrl-c again to quit at once)", "warn"),
            ("Finishing set/set.tar · 10 s", "work"),
        ]
        for line, tone in cases:
            verb = line.split(" ")[0] if not line.startswith(("Not ", "Up ", "Signed ")) else " ".join(line.split(" ")[:2])
            with self.subTest(line=line):
                self.assertIn(f'<span class="fl-{tone}">{verb}</span>', render(f"{verb:>11}{line[len(verb):]}\n"))

    def test_a_dry_run_puts_would_in_the_verb_column_and_its_action_after_it(self):
        self.assertEqual(render("      Would exclude 1 file of pets.zip in data.toml\n"),
                         '      <span class="fl-work">Would exclude</span> 1 file of pets.zip in data.toml\n')

    def test_a_faint_step_is_dim_whole(self):
        for line in ("  Signed in to none of kaggle, zenodo", " Not placed raw · place = false in data.toml"):
            with self.subTest(line=line):
                self.assertEqual(render(line + "\n"), f'<span class="fl-dim">{line}</span>\n')
        self.assertIn('<span class="fl-warn">Not placed</span>',
                      render(" Not placed raw · place = false in data.toml, so `fetchloom sync` removes 2 files\n"))

    def test_lines_under_a_step_are_dim_but_rows_under_a_status_row_are_left_alone(self):
        self.assertEqual(render("    Running clean · python3 clean.py\n            kept 2 of 3 rows\n").splitlines()[1],
                         '            <span class="fl-dim">kept 2 of 3 rows</span>')
        self.assertEqual(render("       Kept emb · a rebuild\n            changed vectors.bin: /home/ada\n").splitlines()[1],
                         "            changed vectors.bin: /home/ada")

    def test_a_search_shows_what_to_type_in_the_accent(self):
        run = ('      Found 12 datasets for "iris"\n'
               "            kaggle:uciml/iris          Iris Species   3.6 KiB\n")
        self.assertEqual(render(run).splitlines()[1],
                         '            <span class="fl-accent">kaggle:uciml/iris</span>          Iris Species   3.6 KiB')

    def test_what_it_cannot_classify_is_left_alone(self):
        # say() lines, questions, answers on stdout and a file listing keep no color
        for line in ("    Sign in hugging face", "      Check the name first at https://huggingface.co/datasets/a/b",
                     "sign in to hugging face now? [y/N] n", "/home/me/work/data.csv", "Iris.csv          5.0 KiB",
                     "data/iris/iris.csv: OK", "  which one?"):
            with self.subTest(line=line):
                self.assertEqual(render(line + "\n"), line + "\n")

    def test_html_in_a_line_is_escaped(self):
        self.assertEqual(render("    Running clean · python3 clean.py <build>/in & <build>/out\n"),
                         '    <span class="fl-done">Running</span> <span class="fl-bold">clean</span>'
                         '<span class="fl-dim"> · python3 clean.py &lt;build&gt;/in &amp; &lt;build&gt;/out</span>\n')

    def test_an_unclosed_backtick_keeps_the_rest_in_the_lines_own_style(self):
        self.assertEqual(render("  hint: run `fetchloom sync\n"), '  <span class="fl-dim">hint: run `fetchloom sync</span>\n')


class Blocks(unittest.TestCase):
    def test_blocks_of_fetchloom_output_are_transcripts(self):
        for text in ("$ fetchloom sync\n", "error: iris is fetched, not built\n", "   Excluded 1 file of pets.zip\n",
                     "  hint: `fetchloom cache gc` frees it\n"):
            with self.subTest(text=text):
                self.assertTrue(is_transcript(text))

    def test_other_text_blocks_are_not(self):
        # a links file, a parts file, settings rows and a file listing from the docs
        for text in ("# inputs for the paper\nhttps://example.org/iris.csv sha256:d144\nzenodo:3608990\n",
                     "fetchloom parts\n8bb11984d17650386b7085dc6c010d62 262144\n",
                     "jobs              32                       settings file  FETCHLOOM_JOBS\n",
                     ".gitattributes    1.5 KiB\nIris.csv          5.0 KiB\n"):
            with self.subTest(text=text):
                self.assertFalse(is_transcript(text))

    def test_only_transcripts_in_a_page_are_colored(self):
        transcript = '<div class="language-text highlight"><pre><span></span><code>     Synced iris\n</code></pre></div>'
        other = '<div class="language-text highlight"><pre><span></span><code>zenodo:3608990\n</code></pre></div>'
        shell = '<div class="language-sh highlight"><pre><span></span><code>fetchloom<span class="w"> </span>sync\n</code></pre></div>'
        page = f"<p>a</p>{transcript}<p>b</p>{other}{shell}"
        self.assertEqual(colored(page), (
            '<p>a</p><div class="language-text highlight fl-transcript"><pre><span></span><code>'
            '     <span class="fl-done">Synced</span> <span class="fl-bold">iris</span>\n</code></pre></div>'
            f"<p>b</p>{other}{shell}"))


if __name__ == "__main__":
    unittest.main()
