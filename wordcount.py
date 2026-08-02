#!/usr/bin/env python3
"""Word count breakdown per source, by volume."""

import json
import sqlite3
import re
import sys
from pathlib import Path

ROOT = Path(__file__).parent
DB_DIR = ROOT / "db"
CONFIG_PATH = ROOT / "config.json"
TAG_RE = re.compile(r"<[^>]+>")


def strip_html(html: str) -> str:
    text = TAG_RE.sub(" ", html)
    return re.sub(r"\s+", " ", text).strip()


def source_names() -> dict[str, str]:
    """Map source id -> display name from config.json."""
    if not CONFIG_PATH.exists():
        return {}
    config = json.loads(CONFIG_PATH.read_text())
    return {s["Id"]: s.get("Name", s["Id"]) for s in config.get("Sources", [])}


def volume_stats(db_path: Path) -> dict[str, dict]:
    conn = sqlite3.connect(db_path)
    try:
        rows = conn.execute("""
            SELECT v.name, rd.data
            FROM chapters c
            JOIN volumes v ON v.id = c.volumeid
            JOIN raw_data rd ON rd.chapter_id = c.id
            ORDER BY v.id, c.id
        """).fetchall()
    finally:
        conn.close()

    volumes: dict[str, dict] = {}
    for vol_name, html in rows:
        stats = volumes.setdefault(vol_name, {"chapters": 0, "words": 0})
        stats["chapters"] += 1
        stats["words"] += len(strip_html(html).split())
    return volumes


def print_source(title: str, volumes: dict[str, dict]) -> tuple[int, int]:
    print(f"\n{title}")
    print("=" * 47)
    print(f"{'Volume':<20} {'Chapters':>10} {'Words':>15}")
    print("-" * 47)

    total_words = 0
    total_chapters = 0
    for vol, stats in volumes.items():
        print(f"{vol:<20} {stats['chapters']:>10} {stats['words']:>15,}")
        total_words += stats["words"]
        total_chapters += stats["chapters"]

    print("-" * 47)
    print(f"{'TOTAL':<20} {total_chapters:>10} {total_words:>15,}")
    print(f"~{total_words / 1_000_000:.1f} million words")
    return total_chapters, total_words


def main():
    names = source_names()
    # Config order first, then any leftover databases on disk.
    on_disk = {p.stem: p for p in sorted(DB_DIR.glob("*.db")) if not p.name.endswith(".bak.db")}
    ordered = [sid for sid in names if sid in on_disk]
    ordered += [sid for sid in on_disk if sid not in names]

    if not ordered:
        print(f"No databases found in {DB_DIR}", file=sys.stderr)
        return 1

    grand_chapters = 0
    grand_words = 0
    for source_id in ordered:
        volumes = volume_stats(on_disk[source_id])
        if not volumes:
            continue
        chapters, words = print_source(names.get(source_id, source_id), volumes)
        grand_chapters += chapters
        grand_words += words

    if len(ordered) > 1:
        print(f"\n{'ALL SOURCES':<20} {grand_chapters:>10} {grand_words:>15,}")
        print(f"~{grand_words / 1_000_000:.1f} million words")
    return 0


if __name__ == "__main__":
    sys.exit(main())
