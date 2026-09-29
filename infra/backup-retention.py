#!/usr/bin/env python3
"""Keep seven daily and four weekly local recovery points, at most 35 days old."""

from __future__ import annotations

import argparse
from datetime import datetime, timedelta, timezone
from pathlib import Path
import re
import shutil


STAMP = re.compile(r"^20\d{6}T\d{6}Z$")
MAX_AGE = timedelta(days=35)


def expired_bundles(root: Path, now: datetime) -> list[Path]:
    if not root.is_absolute() or root.is_symlink() or not root.is_dir():
        raise ValueError("backup root must be an existing, real absolute directory")
    root = root.resolve(strict=True)
    if now.tzinfo is None:
        raise ValueError("current time must be timezone-aware")
    now = now.astimezone(timezone.utc)

    bundles: list[tuple[datetime, Path]] = []
    for child in root.iterdir():
        if not STAMP.fullmatch(child.name) or child.is_symlink() or not child.is_dir():
            continue
        if child.resolve(strict=True).parent != root:
            raise ValueError("backup escaped its root")
        try:
            created = datetime.strptime(child.name, "%Y%m%dT%H%M%SZ").replace(tzinfo=timezone.utc)
        except ValueError:
            continue
        if created > now:
            continue  # Clock skew must not trigger deletion of a future bundle.
        bundles.append((created, child))
    bundles.sort(reverse=True)

    keep: set[Path] = set()
    weekly_weeks = set()
    for created, child in bundles:
        if now - created > MAX_AGE:
            continue
        # Keep manual recovery points too; several may be taken on one day.
        if now - created < timedelta(days=7):
            keep.add(child)
        week = created.isocalendar()[:2]
        if week not in weekly_weeks and len(weekly_weeks) < 4:
            weekly_weeks.add(week)
            keep.add(child)
    return [child for _, child in bundles if child not in keep]


def prune(root: Path, now: datetime, apply: bool) -> list[Path]:
    victims = expired_bundles(root, now)
    if apply:
        resolved_root = root.resolve(strict=True)
        for child in victims:
            # Recheck the direct child immediately before the recursive operation.
            if child.is_symlink() or child.resolve(strict=True).parent != resolved_root:
                raise ValueError("backup changed during retention")
            shutil.rmtree(child)
    return victims


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("--apply", action="store_true", help="delete selected completed bundles")
    args = parser.parse_args()
    victims = prune(args.root, datetime.now(timezone.utc), args.apply)
    print(f"Backup retention: {len(victims)} {'removed' if args.apply else 'would remove'}")


if __name__ == "__main__":
    main()
