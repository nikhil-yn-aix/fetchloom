"""the modules under src/ keep their layers: only cli uses cli and output, and no modules use one
another in a circle, apart from the modules of a group ALLOWED names."""

from pathlib import Path
import functools
import re
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
# net asks source for each source's rules about requests, and auth builds net's credential types
# and masks links by net's rules, until those rules and types sit below net
ALLOWED = [{"auth", "net", "source"}]
WORDS = re.compile(r'//[^\n]*|/\*.*?\*/|(?<!\w)b?r(#*)".*?"\1|b?"(?:\\.|[^"\\])*"|b?\'(?:\\.|[^\'\\])\'', re.S)
TEST = re.compile(r"#\[cfg\((?:test|all\([^\]]*\btest\b[^\]]*)\)\]")
ITEM = re.compile(r"\s*(?:pub(?:\([^)]*\))?\s+)?(?:fn|impl|struct|enum|mod|use|const|static|type|trait|unsafe)\b")
MOD = re.compile(r"((?:#\[[^\]]*\]\s*)*)(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;")


def spaces(text):
    """as many spaces as `text` has characters, keeping its lines."""
    return re.sub(r"[^\n]", " ", text) if "\n" in text else " " * len(text)


def blank(text, spans):
    """`text` with each of `spans`, (start, end), turned to spaces."""
    pieces, last = [], 0
    for start, end in spans:
        pieces += [text[last:start], spaces(text[start:end])]
        last = end
    return "".join(pieces) + text[last:]


def closing(code, at):
    """where the braces opened at `at` close."""
    depth = 0
    while True:
        depth += (code[at] == "{") - (code[at] == "}")
        at += 1
        if not depth:
            return at


def test_only(code):
    """yields (start, end) of each item, field or statement a test-only cfg keeps out of builds."""
    end = 0
    for attribute in TEST.finditer(code):
        if attribute.start() < end:
            continue
        at = attribute.end()
        while more := re.compile(r"\s*#\[[^\]]*\]").match(code, at):
            at = more.end()
        is_item, depth = bool(ITEM.match(code, at)), 0
        while at < len(code):
            char = code[at]
            depth += (char in "([") - (char in ")]")
            if depth < 0 or depth == 0 and char == "}":
                break
            if depth == 0 and char == "{":
                at = closing(code, at)
                break
            at += 1
            if depth == 0 and (char == ";" or char == "," and not is_item):
                break
        end = at
        yield attribute.start(), end


def production(src):
    """{file: its code} for each file a build outside tests compiles, found from lib.rs, with
    comments, strings and test-only code blanked."""
    found, todo = {}, [src / "lib.rs"]
    while todo:
        path = todo.pop()
        text = path.read_text(encoding="utf-8")
        kept = WORDS.sub(lambda word: spaces(word[0]) if word[0].startswith("/") else word[0], text)
        code = WORDS.sub(lambda word: spaces(word[0]), text)
        spans = list(test_only(code))
        kept, code = blank(kept, spans), blank(code, spans)
        found[path] = code
        folder = path.parent if path.name in ("lib.rs", "mod.rs") else path.with_suffix("")
        for declared in MOD.finditer(kept):
            named = re.search(r'#\[path\s*=\s*"([^"]+)"\]', declared[1])
            child = path.parent / named[1] if named else folder / f"{declared[2]}.rs"
            todo.append(child if child.exists() else folder / declared[2] / "mod.rs")
    return found


@functools.cache
def uses(src):
    """{(module, module it uses)} among the top modules of `src`, from their `crate::` paths."""
    files = production(src)
    top = {path: path.relative_to(src).parts[0].removesuffix(".rs") for path in files if path.name != "lib.rs"}
    found = set()
    for path, user in top.items():
        code = files[path]
        for match in re.finditer(r"\bcrate::(\{|\w+)", code):
            names = match[1]
            if names == "{":
                names = code[match.end():closing(code, match.end() - 1) - 1]
                while "{" in names:
                    names = re.sub(r"\{[^{}]*\}", "", names)
            found |= {(user, used) for used in re.findall(r"(?:^|,)\s*(\w+)", names)
                      if used in top.values() and used != user}
    return found


def circles(edges):
    """the groups of modules that use one another in a circle, each sorted."""
    after = {}
    for user, used in edges:
        after.setdefault(user, set()).add(used)
    reach = {}
    for start in after:
        seen, todo = set(), [start]
        while todo:
            for used in after.get(todo.pop(), ()):
                if used not in seen:
                    seen.add(used)
                    todo.append(used)
        reach[start] = seen
    groups = {tuple(sorted(name for name in reach if name in reach[start] and start in reach[name]))
              for start in reach}
    return sorted(list(group) for group in groups if len(group) > 1)


class Reading(unittest.TestCase):
    def test_tests_comments_and_strings_are_not_uses(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        src = Path(folder.name)
        files = {
            "lib.rs": "mod a;\nmod b;\nmod c;\nmod e;\n#[cfg(test)]\nmod d;\n",
            "a.rs": "use crate::{b::{One, two::{Three}}, c};\n// crate::e\nconst Q: char = '\"';\n"
                    "const S: &str = \"crate::e\";\n#[cfg(test)]\nfn helper<A, B>() { crate::e::x(); }\n"
                    "#[cfg(all(test, unix))]\nuse crate::e;\n#[cfg(test)]\nmod tests;\n",
            "a/tests.rs": "use crate::e;\n",
            "b.rs": "struct S {\n    #[cfg(test)]\n    e: crate::e::E,\n    a: crate::a::A,\n}\n",
            "c.rs": "",
            "d.rs": "use crate::a;\n",
            "e.rs": "",
        }
        for name, text in files.items():
            (src / name).parent.mkdir(exist_ok=True)
            (src / name).write_text(text, encoding="utf-8")
        self.assertEqual(uses(src), {("a", "b"), ("a", "c"), ("b", "a")})
        self.assertEqual(circles(uses(src)), [["a", "b"]])


class Layers(unittest.TestCase):
    def test_no_modules_use_one_another_in_a_circle(self):
        found = [group for group in circles(uses(ROOT / "src"))
                 if not any(set(group) <= allowed for allowed in ALLOWED)]
        self.assertEqual(found, [], "move what the modules share to the one that owns it")

    def test_only_cli_uses_cli_and_output(self):
        found = sorted((user, used) for user, used in uses(ROOT / "src") if used in ("cli", "output") and user != "cli")
        self.assertEqual(found, [], "hand values up to cli rather than reaching it from below")


if __name__ == "__main__":
    unittest.main()
