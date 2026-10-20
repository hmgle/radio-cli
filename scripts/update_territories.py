#!/usr/bin/env -S uv run
"""Regenerate Chinese territory names and their license from pinned CLDR 48."""

import json
from pathlib import Path
from urllib.request import urlopen
import xml.etree.ElementTree as ET


REVISION = "acd6d88ae493633240e19a87a721076a8a75c310"
BASE_URL = f"https://raw.githubusercontent.com/unicode-org/cldr/{REVISION}"


def main():
    with urlopen(f"{BASE_URL}/common/main/zh.xml", timeout=30) as response:
        root = ET.fromstring(response.read())
    names = {
        territory.attrib["type"]: territory.text
        for territory in root.findall("./localeDisplayNames/territories/territory")
        if len(territory.attrib["type"]) == 2
        and "alt" not in territory.attrib
        and territory.text
    }
    with urlopen(f"{BASE_URL}/LICENSE", timeout=30) as response:
        license_text = response.read().decode("utf-8")
    locales = Path(__file__).resolve().parent.parent / "locales"
    (locales / "territories.zh-CN.json").write_text(
        json.dumps(names, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    (locales / "LICENSE.unicode").write_text(license_text, encoding="utf-8")


if __name__ == "__main__":
    main()
