from collections import Counter


def top_words(text: str, n: int) -> list[tuple[str, int]]:
    """The n most common words, most common first (ties: alphabetical)."""
    counts = Counter(text.lower().split())
    ranked = sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))
    return ranked[:n]
