import argparse
import sys

from counter import top_words


def main(argv: list[str]) -> list[str]:
    ap = argparse.ArgumentParser(prog="wordcount")
    ap.add_argument("file")
    ap.add_argument("-n", type=int, default=10)
    args = ap.parse_args(argv)
    with open(args.file, encoding="utf-8") as f:
        text = f.read()
    return [f"{word} {count}" for word, count in top_words(text, args.n)]


if __name__ == "__main__":
    print("\n".join(main(sys.argv[1:])))
