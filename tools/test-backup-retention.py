#!/usr/bin/env python3
"""Exercise retention decisions and filesystem boundaries without real backups."""

from datetime import datetime, timedelta, timezone
from pathlib import Path
import importlib.util
import os
import shutil
import tempfile


spec = importlib.util.spec_from_file_location(
    "ldw_backup_retention", Path(__file__).resolve().parents[1] / "infra" / "backup-retention.py"
)
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

now = datetime(2026, 10, 1, 12, tzinfo=timezone.utc)
scratch = Path(os.environ.get("LDW_TEST_TMP", tempfile.gettempdir())).resolve(strict=True)
directory = Path(tempfile.mkdtemp(dir=scratch)).resolve(strict=True)
assert directory.parent == scratch
try:
    root = Path(directory) / "backups"
    root.mkdir()
    for days in range(42):
        stamp = (now - timedelta(days=days)).strftime("%Y%m%dT%H%M%SZ")
        (root / stamp).mkdir()
    newest = (now - timedelta(days=0)).strftime("%Y%m%dT%H%M%SZ")
    extra = (now - timedelta(minutes=30)).strftime("%Y%m%dT%H%M%SZ")
    (root / extra).mkdir()
    (root / ".incomplete.partial").mkdir()
    (root / "notes").write_text("do not remove")
    outside = Path(directory) / "outside"
    outside.mkdir()
    symlink = root / "20260901T010101Z"
    try:
        symlink.symlink_to(outside, target_is_directory=True)
    except OSError:
        if os.name != "nt":
            raise

    victims = module.expired_bundles(root, now)
    victim_names = {child.name for child in victims}
    assert newest not in victim_names
    assert extra in victim_names  # One completed backup per day is retained.
    for days in range(7):
        assert (now - timedelta(days=days)).strftime("%Y%m%dT%H%M%SZ") not in victim_names
    assert (now - timedelta(days=35)).strftime("%Y%m%dT%H%M%SZ") in victim_names
    assert (now - timedelta(days=41)).strftime("%Y%m%dT%H%M%SZ") in victim_names
    assert "20260901T010101Z" not in victim_names
    assert all(child.parent == root for child in victims)
    assert len(victims) > 30
    assert {child.name for child in module.prune(root, now, apply=True)} == victim_names
    assert not (root / extra).exists()
    assert (root / newest).is_dir()
    assert (root / ".incomplete.partial").is_dir()
    assert (root / "notes").read_text() == "do not remove"
    assert outside.is_dir()
finally:
    if directory.parent != scratch:
        raise ValueError("test scratch escaped its root")
    shutil.rmtree(directory)
print("Backup retention selection and boundaries: PASS")
