import unittest

from search import matches
from slug import slugify


class SlugTest(unittest.TestCase):
    def test_plain(self):
        self.assertEqual(slugify("Hello World"), "hello-world")

    def test_accents_and_punctuation(self):
        self.assertEqual(slugify("Héllo, World!"), "hello-world")


class SearchTest(unittest.TestCase):
    # Fails for the same root cause; a fix made only in slug.py leaves it red.
    def test_query_ignores_accents_and_punctuation(self):
        self.assertTrue(matches("cafe", "Meet at the Café."))


if __name__ == "__main__":
    unittest.main()
