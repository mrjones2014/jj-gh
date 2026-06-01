#!/usr/bin/env python3

"""Fetch GitHub's public GraphQL SDL and strip descriptions.

Descriptions are pure docs and ignored by graphql-client's codegen. Otherwise the file is huge.
"""
import pathlib
import re
import sys
import urllib.request

URL = "https://docs.github.com/public/fpt/schema.docs.graphql"
OUT = pathlib.Path.cwd() / "src/gh/github.graphql"

sdl = urllib.request.urlopen(URL).read().decode()
sdl = re.sub(r'"""[\s\S]*?"""\n?', '', sdl)
sdl = re.sub(r'\n\s*\n', '\n', sdl)
OUT.write_text(sdl)
print(f"wrote {OUT} ({len(sdl)} bytes)", file=sys.stderr)
