from normalize import normalize


def slugify(title: str) -> str:
    return "-".join(normalize(title).split())
