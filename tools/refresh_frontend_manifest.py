#!/usr/bin/env python3
"""Rewrite app/frontend.sha256 from the files under app/frontend (run after any front end change)."""
import hashlib, pathlib
root = pathlib.Path(__file__).resolve().parent.parent / "app"
front = root / "frontend"
lines = [f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.relative_to(front).as_posix()}"
         for p in sorted(front.rglob("*")) if p.is_file()]
(root / "frontend.sha256").write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")
print(len(lines), "files")
