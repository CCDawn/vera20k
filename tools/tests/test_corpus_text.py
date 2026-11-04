"""The committed corpus layout native_oracle.finish_vectors writes."""
import json
import unittest

from tools.native_oracle import corpus_text


class CorpusTextTests(unittest.TestCase):
    def test_rows_take_one_line_each_and_read_back_unchanged(self):
        data = {"rows": [{"cell": [1, 2], "rng": [3.5, -0.0]}, {"cell": [3, 4]}],
                "name": "x", "empty": {}, "draws": [7, 8]}
        text = corpus_text(data)
        self.assertEqual(text.splitlines(), [
            '{', ' "rows": [', '  {"cell":[1,2],"rng":[3.5,-0.0]},', '  {"cell":[3,4]}', ' ],',
            ' "name": "x",', ' "empty": {},', ' "draws": [7,8]', '}'])
        self.assertTrue(text.endswith("}\n"))
        self.assertEqual(json.loads(text), data)


if __name__ == "__main__":
    unittest.main()
