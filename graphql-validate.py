#!/usr/bin/env python3
"""Validate GraphQL query documents against a schema SDL file.

Usage: graphql-validate.py <schema.graphql> <doc.gql>...
"""

import pathlib
import sys

from graphql import build_schema, parse, validate


def main() -> int:
    if len(sys.argv) < 3:
        print("usage: graphql-validate.py <schema> <doc>...", file=sys.stderr)
        return 2

    schema = build_schema(pathlib.Path(sys.argv[1]).read_text())

    errors = []
    for doc_path in sys.argv[2:]:
        text = pathlib.Path(doc_path).read_text()
        try:
            doc = parse(text)
        except Exception as e:
            errors.append(f"{doc_path}: parse error: {e}")
            continue
        for err in validate(schema, doc):
            errors.append(f"{doc_path}: {err.message}")

    for e in errors:
        print(e, file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
