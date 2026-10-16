import pathlib
import runpy
import unittest


SCAN = runpy.run_path(str(pathlib.Path(__file__).with_name("check_no_block_comments.py")))["block_comment_lines"]


class BlockCommentTests(unittest.TestCase):
    def test_rejects_inline_and_doc_block_comments(self):
        self.assertEqual(SCAN("fn run() { /* reason */ }\n/** docs */\n/*! module */", ".rs"), [1, 2, 3])
        self.assertEqual(SCAN("SELECT 1; /* SQL */", ".sql"), [1])

    def test_keeps_strings_globs_and_line_comments(self):
        rust = '\n'.join(['let p = "src/*.rs";', 'let p = r###"/* literal */"###;', "let c = '/';", '// /* quoted example', 'fn f<\'a>() { /* actual */ }'])
        self.assertEqual(SCAN(rust, ".rs"), [5])
        sql = "SELECT 'it''s /* data */'; -- /* example\nDO $body$ BEGIN RETURN; END $body$; /* real */"
        self.assertEqual(SCAN(sql, ".sql"), [2])


if __name__ == "__main__":
    unittest.main()
