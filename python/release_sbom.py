"""take the release's sbom from a wheel. maturin writes a cyclonedx sbom of every crate fetchloom
depends on, for any platform, into each wheel it builds, so the x86_64 linux wheel's stands for the
whole release."""

import argparse
import json
from pathlib import Path
import re
import zipfile

from check_artifacts import TARGETS
from ci_journey import require, wheel_version


def release_sbom(dist, destination, version):
    """copy the sbom of the x86_64 linux wheel in `dist` to `destination` as
    fetchloom-<version>.cdx.json, after checking it is a cyclonedx sbom of this version."""
    pattern = re.compile(rf"fetchloom-{re.escape(wheel_version(version))}-py3-none-(.+)\.whl")
    tags = TARGETS["x86_64-unknown-linux-musl"]["wheel"]
    wheels = [path for path in Path(dist).glob("*.whl")
              if (match := pattern.fullmatch(path.name)) and set(match[1].split(".")) == tags]
    require(len(wheels) == 1, f"expected one x86_64 linux wheel in {dist}, found {len(wheels)}")
    with zipfile.ZipFile(wheels[0]) as packed:
        names = [name for name in packed.namelist() if name.endswith(".dist-info/sboms/fetchloom.cyclonedx.json")]
        require(len(names) == 1, f"{wheels[0].name} holds no single sbom")
        data = packed.read(names[0])
    document = json.loads(data)
    component = document.get("metadata", {}).get("component", {})
    require(document.get("bomFormat") == "CycloneDX" and component.get("name") == "fetchloom"
            and component.get("version") == version, f"the sbom in {wheels[0].name} is not one of fetchloom {version}")
    destination = Path(destination)
    destination.mkdir(parents=True, exist_ok=True)
    path = destination / f"fetchloom-{version}.cdx.json"
    path.write_bytes(data)
    return path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dist", type=Path)
    parser.add_argument("destination", type=Path)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    print(f"wrote {release_sbom(args.dist, args.destination, args.version)}")


if __name__ == "__main__":
    main()
