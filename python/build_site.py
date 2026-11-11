"""build the documentation and agent exports without deployment credentials."""

import argparse
from pathlib import Path
import re
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
SITE_URL = "https://nikhil-yn-aix.github.io/fetchloom/"
BLOB = "https://github.com/nikhil-yn-aix/fetchloom/blob/main/"


def relink(match):
    label, url = match.group(1), match.group(2)
    if re.match(r"[a-z]+:", url):
        return match.group(0)
    path, separator, fragment = url.partition("#")
    if path.startswith("docs/") and path.endswith(".md"):
        url = path[len("docs/"):-len(".md")] + ".html" + separator + fragment
    elif not path.startswith("skills/"):
        url = BLOB + url
    return f"[{label}]({url})"


def export_agents(root, site):
    shutil.copytree(root / "skills", site / "skills", dirs_exist_ok=True)
    llms = re.sub(r"\[([^\]]*)\]\(([^)\s]+)\)", relink,
                  (root / "llms.txt").read_text(encoding="utf-8"))
    llms = llms.rstrip("\n") + "\n\n## optional\n\n- [full user guide](llms-full.txt): every user guide page in one file\n"
    (site / "llms.txt").write_text(llms, encoding="utf-8")
    pages = re.findall(r"\]\((user/[^)\s]+\.md)\)", (root / "docs/SUMMARY.md").read_text(encoding="utf-8"))
    if not pages:
        raise ValueError("docs/SUMMARY.md lists no user guide pages")
    parts = [f"# fetchloom user guide\n\nevery page of the fetchloom user guide in reading order, from {SITE_URL}user/README.html\n"]
    for page in pages:
        body = (root / "docs" / page).read_text(encoding="utf-8").strip()
        parts.append(f"---\n\nsource: {SITE_URL}{page[:-len('.md')]}.html\n\n{body}\n")
    (site / "llms-full.txt").write_text("\n".join(parts), encoding="utf-8")
    print(f"llms-full.txt has {len(pages)} pages")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dest-dir", type=Path, default=Path("site"))
    args = parser.parse_args()
    site = args.dest_dir.resolve()
    subprocess.run(["mdbook", "build", "--dest-dir", str(site)], cwd=ROOT, check=True)
    export_agents(ROOT, site)


if __name__ == "__main__":
    main()
