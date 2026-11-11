import os
import tempfile
import unittest

from counter import top_words
from wordcount import main

TEXT = "b a c a b a d"


class WordcountTest(unittest.TestCase):
    def setUp(self):
        fd, self.path = tempfile.mkstemp(suffix=".txt")
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(TEXT)

    def tearDown(self):
        os.remove(self.path)

    def test_default_most_common_first(self):
        self.assertEqual(main([self.path, "-n", "3"]), ["a 3", "b 2", "c 1"])

    def test_reverse_least_common_first(self):
        self.assertEqual(main([self.path, "-n", "3", "--reverse"]), ["c 1", "d 1", "b 2"])

    def test_counter_reverse(self):
        self.assertEqual(top_words(TEXT, 2, reverse=True), [("c", 1), ("d", 1)])


if __name__ == "__main__":
    unittest.main()
