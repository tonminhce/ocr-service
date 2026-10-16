#!/usr/bin/env python3

import bisect
import collections
import io
import pathlib
import re
import subprocess
import sys
import tokenize


SOURCE_ROOTS = {"crates", "scripts", "tools", "ops"}
EXTENSIONS = {".rs", ".sql", ".py", ".sh"}
RAW_STRING = re.compile(r'(?:br|cr|r)(#{0,255})"')
SQL_DOLLAR = re.compile(r'\$(?:[A-Za-z_][A-Za-z_0-9]*)?\$')
RUST_CHAR = re.compile(r"'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'")
HEREDOC = re.compile(r"<<(-?)[ \t]*(?:'([^'\n]+)'|\"([^\"\n]+)\"|([A-Za-z_0-9.-]+))(?=$|[ \t\r\n;&|()<>])")
LINT_CODES = r"[A-Z]+[0-9]+(?:\s*,\s*[A-Z]+[0-9]+)*"
TYPE_CODES = r"[a-z][a-z0-9_-]*(?:\s*,\s*[a-z][a-z0-9_-]*)*"
DIRECTIVE = {
    ".py": re.compile(
        rf"(?:noqa(?:\s*:\s*{LINT_CODES})?|ruff:\s*noqa(?:\s*:\s*{LINT_CODES})?|"
        rf"type:\s*ignore(?:\[{TYPE_CODES}\])?|fmt:\s*(?:on|off|skip)|"
        r"pragma:\s*(?:no cover|no branch))"
    ),
    ".sh": re.compile(r"shellcheck\s+(?:disable|enable)=SC[0-9]+(?:\s*,\s*SC[0-9]+)*"),
    ".sql": re.compile(rf"noqa(?:\s*:\s*(?:(?:disable|enable)=(?:all|{LINT_CODES})|{LINT_CODES}))?"),
}


def quoted_end(source, cursor, quote, sql=False):
    cursor += 1
    while cursor < len(source):
        if source[cursor] == "\\" and not sql:
            cursor += 2
        elif source[cursor] == quote:
            if sql and source.startswith(quote * 2, cursor):
                cursor += 2
            else:
                return cursor + 1
        else:
            cursor += 1
    return len(source)


def compiled_comments(source, suffix):
    cursor = 0
    while cursor < len(source):
        if source.startswith("/*", cursor):
            start = cursor
            cursor += 2
            depth = 1
            while cursor < len(source) and depth:
                if source.startswith("/*", cursor):
                    depth += 1
                    cursor += 2
                elif source.startswith("*/", cursor):
                    depth -= 1
                    cursor += 2
                else:
                    cursor += 1
            yield start, source[start:cursor]
            continue
        if source.startswith("--" if suffix == ".sql" else "//", cursor):
            end = source.find("\n", cursor)
            end = len(source) if end < 0 else end
            yield cursor, source[cursor:end]
            cursor = end
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
        if source[cursor] == '"' or (source[cursor] == "'" and suffix == ".sql"):
            sql_escape = suffix == ".sql" and cursor > 0 and source[cursor - 1] in "Ee" and (cursor < 2 or not (source[cursor - 2].isalnum() or source[cursor - 2] == "_"))
            cursor = quoted_end(source, cursor, source[cursor], sql=suffix == ".sql" and not sql_escape)
            continue
        cursor += 1


def shell_comments(source):
    cursor = 0
    heredocs = []
    while cursor < len(source):
        char = source[cursor]
        if char == "\\":
            cursor += 2
            continue
        if char == "'" and not (cursor > 0 and source[cursor - 1] == "$"):
            end = source.find("'", cursor + 1)
            cursor = len(source) if end < 0 else end + 1
            continue
        if char in "'\"`":
            cursor = quoted_end(source, cursor, char)
            continue
        if source.startswith("${", cursor):
            cursor += 2
            depth = 1
            while cursor < len(source) and depth:
                if source[cursor] == "\\":
                    cursor += 2
                    continue
                depth += (source[cursor] == "{") - (source[cursor] == "}")
                cursor += 1
            continue
        arithmetic = 3 if source.startswith("$((", cursor) else 2 if source.startswith("((", cursor) else 0
        if arithmetic:
            cursor += arithmetic
            depth = 2
            while cursor < len(source) and depth:
                if source[cursor] in "'\"":
                    cursor = quoted_end(source, cursor, source[cursor])
                    continue
                depth += (source[cursor] == "(") - (source[cursor] == ")")
                cursor += 1
            continue
        if source.startswith("<<<", cursor):
            cursor += 3
            continue
        heredoc = HEREDOC.match(source, cursor)
        if heredoc:
            heredocs.append((next(value for value in heredoc.groups()[1:] if value), bool(heredoc[1])))
            cursor = heredoc.end()
            continue
        if char == "#" and (cursor == 0 or source[cursor - 1].isspace() or source[cursor - 1] in ";|&()<>"):
            end = source.find("\n", cursor)
            end = len(source) if end < 0 else end
            yield cursor, source[cursor:end]
            cursor = end
            continue
        if char == "\n" and heredocs:
            cursor += 1
            for delimiter, strip_tabs in heredocs:
                while cursor < len(source):
                    end = source.find("\n", cursor)
                    end = len(source) if end < 0 else end
                    line = source[cursor:end]
                    cursor = min(end + 1, len(source))
                    if (line.lstrip("\t") if strip_tabs else line) == delimiter:
                        break
            heredocs.clear()
            continue
        cursor += 1


def exempt_comment(text, offset, source, suffix):
    if offset == 0 and text.startswith("#!"):
        return True
    body = re.sub(r"^(?://[!/]*|/\*[*!]*|--|#)\s*", "", text)
    body = re.sub(r"(?m)^\s*\*\s?", "", body).lstrip()
    if body.startswith(("SPDX-License-Identifier:", "SPDX-FileCopyrightText:", "Copyright ", "Copyright (")):
        return True
    directive = DIRECTIVE.get(suffix)
    if directive and directive.fullmatch(body.strip()):
        return True
    return suffix == ".py" and source.count("\n", 0, offset) < 2 and bool(re.search(r"coding[:=]\s*[-\w.]+", body))


def comments(source, suffix):
    if suffix == ".py":
        lines = source.splitlines(keepends=True)
        offsets = [0]
        for line in lines:
            offsets.append(offsets[-1] + len(line))
        found = (
            (offsets[token.start[0] - 1] + token.start[1], token.string)
            for token in tokenize.generate_tokens(io.StringIO(source).readline)
            if token.type == tokenize.COMMENT
        )
    else:
        found = shell_comments(source) if suffix == ".sh" else compiled_comments(source, suffix)
    newlines = [index for index, value in enumerate(source) if value == "\n"]
    return [
        (bisect.bisect_left(newlines, offset) + 1, text)
        for offset, text in found
        if not exempt_comment(text, offset, source, suffix)
    ]


def new_comments(current, baseline, suffix):
    available = collections.Counter(text for _, text in comments(baseline, suffix))
    additions = []
    for line, text in comments(current, suffix):
        if available[text]:
            available[text] -= 1
        else:
            additions.append(line)
    return additions


def maintained_source(name):
    path = pathlib.PurePosixPath(name)
    return len(path.parts) > 1 and path.parts[0] in SOURCE_ROOTS and path.suffix in EXTENSIONS


def git(root, *args):
    return subprocess.check_output(["git", *args], cwd=root, stderr=subprocess.PIPE)


def main():
    root = pathlib.Path(__file__).resolve().parent.parent
    try:
        baseline = (root / ".comment-policy-baseline").read_text().strip()
        if not re.fullmatch(r"[0-9a-f]{40}", baseline):
            raise ValueError(".comment-policy-baseline must contain one full commit SHA")
        git(root, "cat-file", "-e", f"{baseline}^{{commit}}")
        previous = set(git(root, "ls-tree", "-rz", "--name-only", baseline).decode().split("\0"))
        names = set(git(root, "ls-files", "-z", "--cached", "--others", "--exclude-standard").decode().split("\0"))
        checked = 0
        failures = []
        for name in sorted(names):
            path = root / name
            if not maintained_source(name) or not path.is_file():
                continue
            checked += 1
            old = git(root, "show", f"{baseline}:{name}").decode() if name in previous else ""
            failures.extend(
                f"{name}:{line}: new code comment is forbidden; use clear names or repository documentation"
                for line in new_comments(path.read_text(), old, path.suffix)
            )
        for failure in failures:
            print(failure, file=sys.stderr)
        print(f"No-new-comments policy: {checked} source files checked against {baseline}, {len(failures)} violations")
        return int(bool(failures))
    except (OSError, ValueError, UnicodeError, tokenize.TokenError, subprocess.CalledProcessError) as error:
        detail = "Git baseline/source unavailable; include full Git history" if isinstance(error, subprocess.CalledProcessError) else str(error)
        print(f"No-new-comments policy could not run: {detail}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
