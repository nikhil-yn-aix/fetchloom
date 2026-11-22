"""native workflows, with preparation kept outside the measured command."""
import json
from pathlib import Path
import shlex
import shutil
import subprocess

import fixtures
import lab

# the scenarios that run again over files already in place, each with its tool's own way to skip work
NOOP = ("s6", "s9")
# the most requests any tool may have in flight to one host, fetchloom's own default
BUDGET = 4
# the control arm's label and settings: the same fetchloom again, under a setting it never reads, so
# the spread between the two is the machine's own noise
CONTROL = ("control", {"FETCHLOOM_EXP_NONE": "1"})


def shell(argv):
    """argv as one shell-quoted line."""
    return shlex.join([str(x) for x in argv])


def script(path, lines):
    """writes lines as a bash script that stops at the first failure; returns the argv that runs it."""
    path.write_text("set -eu\n" + "\n".join(lines) + "\n")
    return ["bash", str(path)]


def reset(path):
    """an empty folder at path, whatever was there."""
    if path.exists():
        shutil.rmtree(path)
    path.mkdir(parents=True)


class Tool:
    """how one tool fetches a case. a subclass names the tool, the scenarios it runs and its command;
    this base lays out the work folder and lists the files a run must leave."""
    name = ""
    scenarios = set(fixtures.ALL_SCENARIOS)
    # what a run's wall and cpu time cover
    clock = "process start to exit"

    def __init__(self, fetchloom):
        self.fetchloom = str(fetchloom)

    def version(self):
        """the first line of --version, or the error that stopped it."""
        try:
            return subprocess.check_output([self.name, "--version"], env=lab.environment(),
                                           stderr=subprocess.STDOUT, text=True).splitlines()[0]
        except (OSError, subprocess.CalledProcessError) as error:
            return str(error)

    def supports(self, scenario):
        return scenario["id"] in self.scenarios

    @property
    def prepared_as(self):
        """the tool whose prepared project this one uses."""
        return self.name

    def cwd(self, workdir):
        return workdir / "project"

    def cache(self, workdir):
        """the tool's own cache: one path for every tool, since each runs alone in its work folder,
        and a longer path costs every lookup more kernel time."""
        return workdir / "cache" / "tool"

    def env(self, workdir):
        env = lab.environment()
        env.update(HOME=str(workdir / "home"), TMPDIR=str(workdir / "scratch"), XDG_CACHE_HOME=str(workdir / "cache"),
                   FETCHLOOM_CACHE=str(self.cache(workdir)), POOCH_CACHE=str(self.cache(workdir)),
                   # dvc keeps its index under /var/tmp otherwise, outside the measured disk
                   DVC_SITE_CACHE_DIR=str(workdir / "cache" / "site"))
        return env

    def prepare(self, scenario, workdir):
        """the work folder before the clock starts; a warm scenario first fetches its base scenario,
        and the next version the version before it, untimed."""
        workdir.mkdir(parents=True, exist_ok=True)
        for folder in ("home", "cache", "project", "scratch"):
            (workdir / folder).mkdir(exist_ok=True)
        if scenario["id"] in fixtures.WARM:
            base = dict(scenario, id=fixtures.WARM[scenario["id"]])
            self.populate(base, workdir)
            self.untimed(base, workdir)
            if scenario["id"] == "s5":
                self.second_project(scenario, workdir)
        elif scenario["id"] in fixtures.NEXT:
            previous = dict(scenario, files=scenario["previous"], version="previous")
            self.populate(previous, workdir)
            self.untimed(previous, workdir)
            self.next_version(scenario, workdir)
        else:
            self.populate(scenario, workdir)

    def next_version(self, scenario, workdir):
        """tells the tool of the dataset's next version, outside the clock, as a git pull of the
        project would; nothing for a tool whose list of files is its command."""

    def untimed(self, scenario, workdir):
        with (workdir / "prepare.log").open("a") as log:
            lab.run_group(self.command(scenario, workdir), cwd=self.cwd(workdir), env=self.env(workdir),
                          stdout=log, stderr=subprocess.STDOUT, timeout=lab.PREPARE_TIMEOUT)

    def second_project(self, scenario, workdir):
        (workdir / "project").rename(workdir / "first-project")
        (workdir / "project").mkdir()
        self.populate(scenario, workdir)

    def populate(self, scenario, workdir):
        (workdir / "project" / "data").mkdir(exist_ok=True)

    def timed(self, result, workdir):
        """a measured run's result, with the clock it covers."""
        return dict(result, clock=self.clock)

    def anchor(self, workdir):
        """what tells when a run's own work began, for a fault to count from, or None when that is
        the command's start."""
        return None

    def outputs(self, scenario, workdir):
        """each file a run must leave, with its sha256."""
        items = scenario["members"] or scenario["files"]
        return [(workdir / "project" / "data" / item["name"], item["sha256"]) for item in items]

    def checks(self, scenario, workdir):
        """writes the expected sums; returns the command that checks them."""
        path = workdir / "SHA256SUMS"
        path.write_text("".join(f'{item["sha256"]}  data/{item["name"]}\n' for item in scenario["files"]))
        return shell(["sha256sum", "-c", path])

    def unpack(self, scenario):
        """the commands that unpack an archive case; none for the others."""
        if scenario["id"] == "s4":
            return [shell(["unzip", "-q", "data/members.zip", "-d", "data"])]
        if scenario["id"] == "s8":
            return [shell(["tar", "-xzf", "data/members.tar.gz", "-C", "data"])]
        return []


class Fetchloom(Tool):
    name = "fetchloom"

    def version(self):
        return subprocess.check_output([self.fetchloom, "--version"], env=lab.environment(), text=True).strip()

    def populate(self, scenario, workdir):
        source = Path(scenario["prepared"]) / self.prepared_as / scenario.get("version", "")
        for name in ("data.toml", "data.lock"):
            shutil.copyfile(source / name, workdir / "project" / name)

    def next_version(self, scenario, workdir):
        # the next version's data.toml and data.lock, as a git pull brings them
        self.populate(scenario, workdir)

    def command(self, scenario, workdir):
        return [self.fetchloom, "-q", "--yes", "sync"]

    def env(self, workdir):
        return {**super().env(workdir), "FETCHLOOM_HOST_JOBS": str(BUDGET)}

    def outputs(self, scenario, workdir):
        if scenario["members"]:
            return [(workdir / "project" / "data" / "f00000" / item["name"], item["sha256"])
                    for item in scenario["members"]]
        return [(workdir / "project" / "data" / f"f{i:05}" / item["name"], item["sha256"])
                for i, item in enumerate(scenario["files"])]


class Variant(Fetchloom):
    """fetchloom with extra environment, measured beside the plain build in the same rounds."""

    def __init__(self, fetchloom, label, settings):
        super().__init__(fetchloom)
        self.name = f"fetchloom+{label}"
        self.settings = dict(settings)

    @property
    def prepared_as(self):
        return Fetchloom.name

    def env(self, workdir):
        return {**super().env(workdir), **self.settings}


def variant(value):
    """parses `label:KEY=VALUE,KEY=VALUE`, where every key is a fetchloom setting."""
    label, _, pairs = value.partition(":")
    if not label or not all(c.islower() or c.isdigit() or c == "-" for c in label):
        raise ValueError("a variant needs a lowercase label before ':'")
    settings = {}
    for pair in pairs.split(","):
        key, sep, setting = pair.partition("=")
        if not sep or not key.startswith("FETCHLOOM_") or key == "FETCHLOOM_CACHE" or key in settings:
            raise ValueError(f"{pair!r} is not a distinct FETCHLOOM_ setting other than the cache")
        settings[key] = setting
    return label, settings


class Curl(Tool):
    name = "curl"
    scenarios = Tool.scenarios - {"s5"}

    def command(self, scenario, workdir):
        return script(workdir / "command.sh", self.steps(scenario, workdir))

    def steps(self, scenario, workdir):
        argv = ["curl", "-q", "--parallel", "--parallel-max", str(BUDGET)]
        for i, item in enumerate(scenario["files"]):
            if i:
                argv.append("--next")
            # each transfer after --next starts without the options before it
            argv.append("-L")
            if scenario["id"] in NOOP:
                argv.extend(["-z", "data/" + item["name"]])
            if scenario["id"] == "s7":
                argv.extend(["-C", "-"])
            argv.extend(["--output", "data/" + item["name"], item["url"]])
        return [shell(["cd", workdir / "project"]), shell(argv), self.checks(scenario, workdir),
                *self.unpack(scenario)]


class DurableCurl(Curl):
    """curl as above, then a flush of the filesystem it wrote, as fetchloom flushes the files it
    places, so the cost of making bytes survive a power cut is on both sides; only in the cases that
    write data."""
    name = "curl+sync"
    scenarios = Curl.scenarios - set(NOOP) - set(fixtures.NEXT)

    def version(self):
        return Curl("unused").version()

    @property
    def prepared_as(self):
        return Curl.name

    def steps(self, scenario, workdir):
        return [*super().steps(scenario, workdir), shell(["sync", "-f", "data"])]


class Aria2c(Tool):
    name = "aria2c"
    scenarios = Curl.scenarios

    def command(self, scenario, workdir):
        urls = workdir / "aria2.txt"
        urls.write_text("".join(f'{item["url"]}\n  out={item["name"]}\n  checksum=sha-256={item["sha256"]}\n'
                                for item in scenario["files"]))
        # -x splits only the large file, into BUDGET pieces
        argv = ["aria2c", "-j", str(BUDGET), "-x", str(BUDGET), "-i", urls, "-d", "data"]
        # its documented check of files already there: a file whose hash is wrong is fetched again
        if scenario["id"] in NOOP or scenario["id"] in fixtures.NEXT:
            argv.append("--check-integrity=true")
        return script(workdir / "command.sh", [shell(["cd", workdir / "project"]), shell(argv),
                                               *self.unpack(scenario)])


class Pooch(Tool):
    """pooch as a notebook uses it: retrieve calls in an interpreter that has already imported it.
    the script times the calls itself, wall and cpu, and the import beside them."""
    name = "pooch"
    clock = "call inside a warm interpreter"
    # what the timed calls report, from inside the process, and when they began
    CALLS = "calls.json"
    BEGIN = "calls.begin"

    def version(self):
        return subprocess.check_output([str(lab.VENV / "bin" / "python"), "-c",
                                        "import pooch; print(pooch.__version__)"], env=lab.environment(), text=True).strip()

    def command(self, scenario, workdir):
        registry = workdir / "registry.json"
        registry.write_text(json.dumps(scenario["files"]))
        processor = {"s4": "pooch.Unzip(extract_dir='unpacked')",
                     "s8": "pooch.Untar(extract_dir='unpacked')"}.get(scenario["id"], "None")
        source = workdir / "retrieve.py"
        # the report is written even when ctrl-c stops the calls, as s7's first attempt does
        source.write_text("import json, os, resource, time\n"
                          "started = time.perf_counter()\n"
                          "import pooch\n"
                          "imported = time.perf_counter()\n"
                          f"items = json.load(open({str(registry)!r}))\n"
                          f"with open({str(workdir / self.BEGIN) + '.new'!r}, 'w') as mark:\n"
                          "    mark.write(repr(time.monotonic()))\n"
                          f"os.replace({str(workdir / self.BEGIN) + '.new'!r}, {str(workdir / self.BEGIN)!r})\n"
                          "before, begun = resource.getrusage(resource.RUSAGE_SELF), time.perf_counter()\n"
                          "try:\n"
                          "    for item in items:\n"
                          "        pooch.retrieve(url=item['url'], known_hash='sha256:' + item['sha256'],\n"
                          "                       fname=item['name'], path=os.environ['POOCH_CACHE'],\n"
                          f"                       processor={processor})\n"
                          "finally:\n"
                          "    ended, after = time.perf_counter(), resource.getrusage(resource.RUSAGE_SELF)\n"
                          f"    with open({str(workdir / self.CALLS)!r}, 'w') as report:\n"
                          "        json.dump(dict(import_seconds=imported - started, wall_seconds=ended - begun,\n"
                          "                       user_seconds=after.ru_utime - before.ru_utime,\n"
                          "                       sys_seconds=after.ru_stime - before.ru_stime), report)\n")
        return [str(lab.VENV / "bin" / "python"), str(source)]

    def timed(self, result, workdir):
        """the result with its clock moved to the calls: their wall and cpu time from the script's
        report, the whole process's beside them and the import time; the process's alone, with its
        clock, when the script left no report."""
        report = workdir / self.CALLS
        (workdir / self.BEGIN).unlink(missing_ok=True)
        try:
            calls = json.loads(report.read_text())
            report.unlink()
        except (OSError, ValueError):
            return dict(result, clock=Tool.clock)
        process = {key: result[key] for key in ("wall_seconds", "user_seconds", "sys_seconds")}
        return dict(result, clock=self.clock, process=process, import_seconds=calls["import_seconds"],
                    **{key: calls[key] for key in ("wall_seconds", "user_seconds", "sys_seconds")})

    def anchor(self, workdir):
        """the moment the script marked as its calls began, a time.monotonic() reading every process
        on the machine shares, so ctrl-c and a dropped link meet pooch's calls, not its import."""
        mark = workdir / self.BEGIN

        def began():
            try:
                return float(mark.read_text())
            except (OSError, ValueError):
                return None
        return began

    def outputs(self, scenario, workdir):
        root = self.cache(workdir)
        if scenario["members"]:
            root /= "unpacked"
        return [(root / item["name"], item["sha256"]) for item in scenario["members"] or scenario["files"]]


class Dvc(Tool):
    name = "dvc"
    # dvc and datalad track files, not archives
    scenarios = Tool.scenarios - {"s4", "s8"}

    def populate(self, scenario, workdir):
        shutil.copytree(Path(scenario["prepared"]) / self.name, workdir / "project", dirs_exist_ok=True)
        subprocess.run(["dvc", "config", "cache.dir", str(self.cache(workdir))],
                       cwd=workdir / "project", env=self.env(workdir), check=True, capture_output=True)
        if scenario.get("version"):
            self.checkout(scenario["version"], workdir)

    def next_version(self, scenario, workdir):
        # a git pull brings the next version's data.dvc, and dvc pull fetches what it lacks
        self.checkout("next", workdir)

    def checkout(self, version, workdir):
        subprocess.run(["git", "checkout", "-q", version], cwd=workdir / "project", env=self.env(workdir),
                       check=True, capture_output=True)

    def command(self, scenario, workdir):
        return ["dvc", "pull", "-j", str(BUDGET)]


class Datalad(Tool):
    name = "datalad"
    scenarios = Dvc.scenarios

    def populate(self, scenario, workdir):
        # like DVC's project copy, cloning the prepared dataset is setup, not retrieval
        self.setup(["datalad", "clone", Path(scenario["prepared"]) / self.name, workdir / "project"], workdir, workdir)
        if scenario.get("version"):
            self.setup(["git", "reset", "-q", "--hard", scenario["version"]], workdir)

    def next_version(self, scenario, workdir):
        # datalad's own update merges what the dataset's sibling has, and get fetches what it lacks
        self.setup(["datalad", "update", "--how", "merge"], workdir)

    def setup(self, argv, workdir, cwd=None):
        """runs an untimed step in the project, or in cwd, logging it beside the preparation."""
        with (workdir / "prepare.log").open("a") as log:
            lab.run_group(argv, cwd=cwd or workdir / "project", env=self.env(workdir), stdout=log, stderr=subprocess.STDOUT,
                          timeout=lab.PREPARE_TIMEOUT)

    def second_project(self, scenario, workdir):
        # a second project is a clone of the first, made before the clock starts as DVC's copy is
        (workdir / "project").rename(workdir / "first-project")
        self.setup(["datalad", "clone", workdir / "first-project", workdir / "project"], workdir, workdir)

    def command(self, scenario, workdir):
        lines = [shell(["cd", workdir / "project"]),
                 f"git config annex.security.allowed-ip-addresses {lab.SERVER_ADDRESS}", f"datalad get -J {BUDGET} data"]
        return script(workdir / "command.sh", lines)


CLASSES = (Fetchloom, Curl, DurableCurl, Aria2c, Pooch, Dvc, Datalad)


def available(fetchloom, variants=(), only=None):
    """the tool adapters by name: every tool, or those in only, plus one per variant."""
    adapters = {cls.name: cls(fetchloom) for cls in CLASSES if only is None or cls.name in only}
    for label, settings in variants:
        tool = Variant(fetchloom, label, settings)
        adapters[tool.name] = tool
    return adapters
