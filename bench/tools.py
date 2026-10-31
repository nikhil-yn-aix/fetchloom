"""Native workflows, with preparation kept outside the measured command."""
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess

import lab


def shell(argv):
    return shlex.join([str(x) for x in argv])


def script(path, lines):
    path.write_text("set -eu\n" + "\n".join(lines) + "\n")
    return ["bash", str(path)]


def reset(path):
    if path.exists():
        shutil.rmtree(path)
    path.mkdir(parents=True)


class Tool:
    name = ""
    scenarios = {f"s{i}" for i in range(1, 8)}

    def __init__(self, fetchloom):
        self.fetchloom = str(fetchloom)

    def version(self):
        try:
            return subprocess.check_output([self.name, "--version"], env=lab.environment(),
                                           stderr=subprocess.STDOUT, text=True).splitlines()[0]
        except (OSError, subprocess.CalledProcessError) as error:
            return str(error)

    def supports(self, scenario):
        return scenario["id"] in self.scenarios

    def cwd(self, workdir):
        return workdir

    def env(self, workdir):
        env = lab.environment()
        env.update(HOME=str(workdir / "home"), TMPDIR=str(workdir / "scratch"), XDG_CACHE_HOME=str(workdir / "cache"),
                   FETCHLOOM_CACHE=str(workdir / "cache" / "fetchloom"),
                   POOCH_CACHE=str(workdir / "cache" / "pooch"))
        return env

    def prepare(self, scenario, workdir, cold=True):
        if cold:
            reset(workdir)
        workdir.mkdir(parents=True, exist_ok=True)
        for folder in ("home", "cache", "project", "scratch"):
            (workdir / folder).mkdir(exist_ok=True)
        if scenario["id"] in ("s5", "s6"):
            base = dict(scenario, id="s2")
            self.populate(base, workdir)
            self.untimed(base, workdir)
            if scenario["id"] == "s5":
                self.second_project(scenario, workdir)
        else:
            self.populate(scenario, workdir)

    def untimed(self, scenario, workdir):
        with (workdir / "prepare.log").open("a") as log:
            subprocess.run(self.command(scenario, workdir), cwd=self.cwd(workdir), env=self.env(workdir),
                           stdout=log, stderr=subprocess.STDOUT, check=True, timeout=1200)

    def second_project(self, scenario, workdir):
        (workdir / "project").rename(workdir / "first-project")
        (workdir / "project").mkdir()
        self.populate(scenario, workdir)

    def populate(self, scenario, workdir):
        (workdir / "project" / "data").mkdir(exist_ok=True)

    def outputs(self, scenario, workdir):
        items = scenario["members"] or scenario["files"]
        return [(workdir / "project" / "data" / item["name"], item["sha256"]) for item in items]

    def checks(self, scenario, workdir):
        path = workdir / "SHA256SUMS"
        path.write_text("".join(f'{item["sha256"]}  data/{item["name"]}\n' for item in scenario["files"]))
        return shell(["sha256sum", "-c", path])

    def unpack(self, scenario):
        if scenario["id"] == "s4":
            return [shell(["unzip", "-q", "data/members.zip", "-d", "data"])]
        return []


class Fetchloom(Tool):
    name = "fetchloom"

    def version(self):
        return subprocess.check_output([self.fetchloom, "--version"], text=True).strip()

    def populate(self, scenario, workdir):
        for name in ("data.toml", "data.lock"):
            shutil.copyfile(Path(scenario["prepared"]) / self.name / name, workdir / "project" / name)

    def cwd(self, workdir):
        return workdir / "project"

    def command(self, scenario, workdir):
        return [self.fetchloom, "-q", "--yes", "sync"]

    def outputs(self, scenario, workdir):
        if scenario["members"]:
            return [(workdir / "project" / "data" / "f00000" / item["name"], item["sha256"])
                    for item in scenario["members"]]
        return [(workdir / "project" / "data" / f"f{i:05}" / item["name"], item["sha256"])
                for i, item in enumerate(scenario["files"])]


class Curl(Tool):
    name = "curl"
    scenarios = Tool.scenarios - {"s5"}

    def command(self, scenario, workdir):
        argv = ["curl", "--parallel", "--parallel-max", "4"]
        for i, item in enumerate(scenario["files"]):
            if i:
                argv.append("--next")
            if scenario["id"] == "s6":
                argv.extend(["-z", "data/" + item["name"]])
            if scenario["id"] == "s7":
                argv.extend(["-C", "-"])
            argv.extend(["--output", "data/" + item["name"], item["url"]])
        return script(workdir / "command.sh", [shell(["cd", workdir / "project"]), shell(argv),
                                               self.checks(scenario, workdir), *self.unpack(scenario)])


class Wget(Tool):
    name = "wget"
    scenarios = Curl.scenarios

    def command(self, scenario, workdir):
        urls = workdir / "urls.txt"
        urls.write_text("".join(item["url"] + "\n" for item in scenario["files"]))
        argv = ["wget", "-i", urls, "-P", "data"]
        if scenario["id"] == "s6":
            argv.append("-nc")
        if scenario["id"] == "s7":
            argv.append("-c")
        return script(workdir / "command.sh", [shell(["cd", workdir / "project"]), shell(argv),
                                               self.checks(scenario, workdir), *self.unpack(scenario)])


class Aria2c(Tool):
    name = "aria2c"
    scenarios = Curl.scenarios

    def command(self, scenario, workdir):
        urls = workdir / "aria2.txt"
        urls.write_text("".join(f'{item["url"]}\n  out={item["name"]}\n  checksum=sha-256={item["sha256"]}\n'
                                for item in scenario["files"]))
        argv = ["aria2c", "-j", "4", "-x", "4", "-i", urls, "-d", "data"]
        if scenario["id"] == "s6":
            argv.append("--check-integrity=true")
        return script(workdir / "command.sh", [shell(["cd", workdir / "project"]), shell(argv),
                                               *self.unpack(scenario)])


class Pooch(Tool):
    name = "pooch"

    def version(self):
        return subprocess.check_output(["/opt/fl-bench/venv/bin/python", "-c",
                                        "import pooch; print(pooch.__version__)"], text=True).strip()

    def command(self, scenario, workdir):
        registry = workdir / "registry.json"
        registry.write_text(json.dumps(scenario["files"]))
        processor = "pooch.Unzip(extract_dir='unpacked')" if scenario["id"] == "s4" else "None"
        source = workdir / "retrieve.py"
        source.write_text("import json, os, pooch\n"
                          f"for item in json.load(open({str(registry)!r})):\n"
                          "    pooch.retrieve(url=item['url'], known_hash='sha256:' + item['sha256'],\n"
                          "                   fname=item['name'], path=os.environ['POOCH_CACHE'],\n"
                          f"                   processor={processor})\n")
        return ["/opt/fl-bench/venv/bin/python", str(source)]

    def outputs(self, scenario, workdir):
        root = workdir / "cache" / "pooch"
        if scenario["members"]:
            root /= "unpacked"
        return [(root / item["name"], item["sha256"]) for item in scenario["members"] or scenario["files"]]


class Dvc(Tool):
    name = "dvc"
    scenarios = Tool.scenarios - {"s4"}

    def populate(self, scenario, workdir):
        shutil.copytree(Path(scenario["prepared"]) / self.name, workdir / "project", dirs_exist_ok=True)
        subprocess.run(["dvc", "config", "cache.dir", str(workdir / "cache" / "dvc")],
                       cwd=workdir / "project", env=self.env(workdir), check=True, capture_output=True)

    def cwd(self, workdir):
        return workdir / "project"

    def command(self, scenario, workdir):
        return ["dvc", "pull", "-j", "4"]


class Datalad(Tool):
    name = "datalad"
    scenarios = Dvc.scenarios

    def populate(self, scenario, workdir):
        pass

    def second_project(self, scenario, workdir):
        (workdir / "project").rename(workdir / "first-project")
        (workdir / "project").mkdir()

    def command(self, scenario, workdir):
        lines = []
        if not (workdir / "project" / ".git").exists():
            source = workdir / "first-project" if scenario["id"] == "s5" else Path(scenario["prepared"]) / self.name
            lines.append(shell(["datalad", "clone", source, workdir / "project"]))
        lines += [shell(["cd", workdir / "project"]),
                  "git config annex.security.allowed-ip-addresses 10.77.0.2", "datalad get -J 4 data"]
        return script(workdir / "command.sh", lines)


CLASSES = (Fetchloom, Curl, Aria2c, Wget, Pooch, Dvc, Datalad)


def available(fetchloom):
    return {cls.name: cls(fetchloom) for cls in CLASSES}
