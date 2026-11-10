from normalize import normalize


def matches(query: str, document: str) -> bool:
    """True when every query word appears as a word of the document."""
    words = set(normalize(document).split())
    return all(w in words for w in normalize(query).split())
