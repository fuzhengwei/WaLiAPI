#!/usr/bin/env python3
"""只读检查旧库迁移校验值，区分行尾变化与无法识别的内容变化。"""

import argparse
import hashlib
import json
from pathlib import Path
import sqlite3


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database", required=True, type=Path)
    args = parser.parse_args()
    database = args.database.resolve(strict=True)
    migrations = Path(__file__).resolve().parents[1] / "src-tauri" / "migrations"
    with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True, timeout=5) as connection:
        connection.execute("PRAGMA query_only = ON")
        applied = dict(connection.execute(
            "SELECT version, checksum FROM _sqlx_migrations WHERE success = 1 ORDER BY version"
        ))

    compiled = {}
    for path in migrations.glob("*.sql"):
        compiled[int(path.name.split("_", 1)[0])] = path.read_bytes()
    line_ending_only = []
    unrecognized = []
    compatible = []
    for version, checksum in sorted(applied.items()):
        raw = compiled.get(version)
        if raw is None:
            unrecognized.append(version)
            continue
        if hashlib.sha384(raw).digest() == checksum:
            compatible.append(version)
            continue
        lf = raw.replace(b"\r\n", b"\n")
        if checksum in (hashlib.sha384(lf).digest(), hashlib.sha384(lf.replace(b"\n", b"\r\n")).digest()):
            line_ending_only.append(version)
        else:
            unrecognized.append(version)

    mismatches = sorted(line_ending_only + unrecognized)
    print(json.dumps({
        "database_schema_version": max(applied, default=0),
        "source_schema_version": max(compiled, default=0),
        "compatible_versions": compatible,
        "line_ending_only_versions": line_ending_only,
        "unrecognized_versions": unrecognized,
        "first_incompatible_version": mismatches[0] if mismatches else None,
    }, indent=2))
    return int(bool(mismatches))


if __name__ == "__main__":
    raise SystemExit(main())
