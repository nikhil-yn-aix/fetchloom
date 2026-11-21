"""makes lead 14's red or break tree from the fixed one: red looks at nothing at the name, as main
does, and break looks through the link, as a wrong fix would."""
import sys
from pathlib import Path

mode = sys.argv[1]
put = Path("src/cli/sync/place/put.rs")
text = put.read_text()
fixed = "std::fs::symlink_metadata(path).is_ok_and(|found| found.file_type().is_symlink())"
assert text.count(fixed) == 1, "the fix is not where it was"
if mode == "red":
    text = text.replace(fixed, "{\n        let _ = path;\n        false\n    }")
elif mode == "break":
    text = text.replace(fixed, "std::fs::metadata(path).is_ok_and(|found| found.file_type().is_symlink())")
put.write_text(text)
print(f"{mode} tree made")
