#!/usr/bin/env python3

import pathlib
import re
import subprocess
import sys


EXTENSIONS = {".rs", ".sql", ".c", ".h", ".cpp", ".hpp", ".js", ".ts", ".css"}
RAW_STRING = re.compile(r'(?:br|r)(#{0,255})"')
SQL_DOLLAR = re.compile(r'\$(?:[A-Za-z_][A-Za-z_0-9]*)?\$')
RUST_CHAR = re.compile(r"'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'")


def block_comment_lines(source, suffix):
    result = []
    cursor = 0
    while cursor < len(source):
        if source.startswith("/*", cursor):
            result.append(source.count("\n", 0, cursor) + 1)
            end = source.find("*/", cursor + 2)
            cursor = len(source) if end < 0 else end + 2
            continue
        if source.startswith("//", cursor) or (suffix == ".sql" and source.startswith("--", cursor)):
            end = source.find("\n", cursor)
            cursor = len(source) if end < 0 else end + 1
            continue
        raw = RAW_STRING.match(source, cursor) if suffix == ".rs" else None
        dollar = SQL_DOLLAR.match(source, cursor) if suffix == ".sql" else None
        if raw or dollar:
            match = raw or dollar
            delimiter = '"' + raw.group(1) if raw else dollar.group()
            end = source.find(delimiter, match.end())
            cursor = len(source) if end < 0 else end + len(delimiter)
            continue
        char = RUST_CHAR.match(source, cursor) if suffix == ".rs" else None
        if char:
            cursor = char.end()
            continue
        quote = source[cursor]
        if quote == '"' or (quote == "'" and suffix != ".rs") or (quote == "`" and suffix in {".js", ".ts"}):
            cursor += 1
            while cursor < len(source):
                if source[cursor] == "\\" and suffix != ".sql":
                    cursor += 2
                elif source[cursor] == quote:
                    if suffix == ".sql" and source.startswith(quote * 2, cursor):
                        cursor += 2
                        continue
                    cursor += 1
                    break
                else:
                    cursor += 1
            continue
        cursor += 1
    return result


def main():
    root = pathlib.Path(__file__).resolve().parent.parent
    names = subprocess.check_output(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"], cwd=root,
    ).decode().split("\0")
    checked = 0
    failures = []
    for name in sorted(set(names)):
        path = root / name
        if path.suffix not in EXTENSIONS or not path.is_file():
            continue
        checked += 1
        failures.extend(f"{name}:{line}: block comment is forbidden" for line in block_comment_lines(path.read_text(), path.suffix))
    for failure in failures:
        print(failure, file=sys.stderr)
    print(f"Block-comment policy: {checked} source files checked, {len(failures)} violations")
    return int(bool(failures))


if __name__ == "__main__":
    raise SystemExit(main())
