import unittest

import check_no_new_comments as policy


class NoNewCommentsTests(unittest.TestCase):
    def test_rejects_rust_line_doc_and_nested_block_comments(self):
        source = "// line\n/// doc\n//! module\n/* outer /* nested */ tail */\n"
        self.assertEqual(policy.new_comments(source, "", ".rs"), [1, 2, 3, 4])

    def test_rust_strings_raw_bytes_c_strings_chars_and_lifetimes_are_not_comments(self):
        source = '''let x = "// string /* marker */";
let a = r###"// raw /* marker */"###;
let b = br#"/* bytes */"#;
let c = cr#"// c string"#;
let d = '\\''; let e = '/'; let f: &'a str = "ok";
'''
        self.assertEqual(policy.new_comments(source, "", ".rs"), [])

    def test_old_comments_survive_code_edits_and_movement_within_the_same_file(self):
        self.assertEqual(policy.new_comments("let a = 2;\n\n// old\n", "// old\nlet a = 1;\n", ".rs"), [])

    def test_changed_and_duplicated_old_comments_are_new(self):
        self.assertEqual(policy.new_comments("// old\n// old\n// changed\n", "// old\n", ".rs"), [2, 3])

    def test_removed_comments_are_allowed(self):
        self.assertEqual(policy.new_comments("let x = 1;\n", "// old\nlet x = 1;\n", ".rs"), [])

    def test_python_tokenizer_distinguishes_comments_from_string_values(self):
        source = 'x = "# value"\ny = """# multiline\n# string"""\n# new\nx = 1  # trailing\n'
        self.assertEqual(policy.new_comments(source, "", ".py"), [4, 5])

    def test_sql_quoted_and_dollar_quoted_values_are_not_comments(self):
        source = '''SELECT '-- value', 'it''s /* quoted */', "--identifier", $tag$-- value$tag$;
-- new
SELECT 1; /* new */
'''
        self.assertEqual(policy.new_comments(source, "", ".sql"), [2, 3])

    def test_sql_escape_strings_preserve_escaped_quotes(self):
        source = "SELECT E'escaped\\' -- literal', E'it''s /* literal */'; -- new\n"
        self.assertEqual(policy.new_comments(source, "", ".sql"), [1])

    def test_shell_parameter_expansion_words_strings_and_escapes_are_not_comments(self):
        source = '''printf '%s' "$#" "${x# #pattern}" "# string" '# literal' word#tail \\#escaped
echo ${x##prefix} ${x%%suffix}
# new
echo ok;# trailing
'''
        self.assertEqual(policy.new_comments(source, "", ".sh"), [3, 4])

    def test_shell_heredoc_contents_are_data_and_later_comment_is_rejected(self):
        source = "cat <<'DATA'\n# content\nDATA\ncat <<-OTHER\n\t# content\n\tOTHER\n# new\n"
        self.assertEqual(policy.new_comments(source, "", ".sh"), [7])

    def test_shell_punctuated_delimiters_are_consumed_as_complete_words(self):
        source = 'cat <<EOF-DATA\n# literal\nEOF-DATA\n# after first\ncat <<EOF.data\n# literal\nEOF.data\n# after second\n'
        self.assertEqual(policy.new_comments(source, "", ".sh"), [4, 8])

    def test_shell_unsupported_mixed_delimiters_cannot_swallow_later_comments(self):
        source = 'cat <<EOF"DATA"\nliteral\nEOFDATA\n# new explanation\n'
        self.assertEqual(policy.new_comments(source, "", ".sh"), [4])

    def test_shell_here_string_does_not_swallow_later_comments(self):
        source = 'value=$(cat <<< "data")\n# new explanation\ncat <<< "$value" # trailing\n'
        self.assertEqual(policy.new_comments(source, "", ".sh"), [2, 3])

    def test_shell_arithmetic_shifts_do_not_swallow_later_comments(self):
        source = 'value=$((1 << bits))\n# after expansion\n((mask <<= bits)) # after command\nvalue=$(((one + two) << bits))\n# after nested expression\n'
        self.assertEqual(policy.new_comments(source, "", ".sh"), [2, 3, 5])

    def test_shell_single_quote_backslash_is_literal_and_ansi_quote_escape_is_not(self):
        source = "echo 'slash\\' # new\necho $'escaped\\' # literal' # new\n"
        self.assertEqual(policy.new_comments(source, "", ".sh"), [1, 2])

    def test_common_tool_directives_and_multiline_license_header_are_exempt(self):
        self.assertEqual(policy.new_comments("# shellcheck disable=SC2086\n", "", ".sh"), [])
        self.assertEqual(policy.new_comments("SELECT 1; -- noqa: L001\n", "", ".sql"), [])
        self.assertEqual(policy.new_comments("/*\n * Copyright (c) Example\n * Licensed under MIT.\n */\n", "", ".rs"), [])

    def test_tool_directives_are_exempt_only_in_their_own_language(self):
        for source in ['// noqa: this explains the algorithm\n', '// fmt: explanation\n', '// shellcheck disable=SC2086\n']:
            self.assertEqual(policy.new_comments(source, "", ".rs"), [1])
        self.assertEqual(policy.new_comments('# noqa: explanation\n', "", ".sh"), [1])
        self.assertEqual(policy.new_comments('# shellcheck disable=SC2086\n', "", ".py"), [1])
        self.assertEqual(policy.new_comments('-- shellcheck disable=SC2086\n', "", ".sql"), [1])

    def test_supported_directives_accept_only_exact_forms_and_code_lists(self):
        for source in [
            '# fmt: on\n', '# fmt: off\n', '# fmt: skip\n', '# noqa\n', '# noqa: E501, F401\n',
            '# ruff: noqa: RUF100, F401\n', '# type: ignore[attr-defined, union-attr]\n', '# pragma: no cover\n',
        ]:
            self.assertEqual(policy.new_comments(source, "", ".py"), [], source)
        self.assertEqual(policy.new_comments('# shellcheck disable=SC2086,SC1090\n', "", ".sh"), [])
        self.assertEqual(policy.new_comments('-- noqa: disable=LT01, CP01\n', "", ".sql"), [])

    def test_directive_prose_payloads_and_unknown_directives_are_rejected(self):
        for source in [
            '# fmt: this explains the algorithm\n', '# noqa: explanation\n', '# noqa: F401 explanation\n',
            '# ruff: explanation\n', '# type: ignore this explains\n', '# pragma: no cover explanation\n',
            '# unknown: off\n',
        ]:
            self.assertEqual(policy.new_comments(source, "", ".py"), [1], source)
        self.assertEqual(policy.new_comments('# shellcheck disable=SC2086 explanation\n', "", ".sh"), [1])
        self.assertEqual(policy.new_comments('-- noqa: disable=explanation\n', "", ".sql"), [1])

    def test_shebang_encoding_tool_directives_and_license_notices_are_exempt(self):
        python = "#!/usr/bin/env python3\n# coding: utf-8\n# SPDX-License-Identifier: MIT\nx = 1  # noqa: E123\n# fmt: off\n"
        shell = "#!/bin/sh\n# shellcheck disable=SC2086\n# Copyright (c) 2026 Example\n"
        rust = "// SPDX-License-Identifier: Apache-2.0\n/* Copyright (c) Example */\n"
        self.assertEqual(policy.new_comments(python, "", ".py"), [])
        self.assertEqual(policy.new_comments(shell, "", ".sh"), [])
        self.assertEqual(policy.new_comments(rust, "", ".rs"), [])

    def test_only_maintained_source_roots_and_supported_languages_are_checked(self):
        for name in ["crates/a/src/lib.rs", "crates/a/migrations/0007.sql", "scripts/a.sh", "tools/a.py", "ops/redis.sh"]:
            self.assertTrue(policy.maintained_source(name), name)
        for name in ["docs/a.py", ".claude/a.py", "licenses/a.rs", "ops/config.yaml", "target/generated.rs", "README.md"]:
            self.assertFalse(policy.maintained_source(name), name)


if __name__ == "__main__":
    unittest.main()
