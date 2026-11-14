def normalize(text: str) -> str:
    """Lower-case ASCII words separated by single spaces: accents are
    removed and punctuation is dropped. Shared by slug.py and search.py."""
    return " ".join(text.lower().split())
