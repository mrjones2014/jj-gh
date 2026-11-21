"""mdBook preprocessor: replace the reference chapter with generated CLI help."""

import json
import subprocess
import sys

if len(sys.argv) > 1 and sys.argv[1] == "supports":
    sys.exit(0)

_context, book = json.load(sys.stdin)
reference = subprocess.check_output(["gen-docs"], text=True)
found = False


def visit(items):
    global found
    for item in items:
        chapter = item.get("Chapter")
        if chapter is None:
            continue
        if chapter.get("path") == "reference/cli.md":
            chapter["content"] = reference
            found = True
        visit(chapter.get("sub_items", []))


visit(book.get("items", book.get("sections", [])))
if not found:
    sys.exit("CLI reference chapter not found in SUMMARY.md")
json.dump(book, sys.stdout)
