import json
from pathlib import Path
from typing import Any

import pytest
from conftest import load_tool


@pytest.mark.parametrize("caught,missed,timeout,score", [(8, 2, 0, (8, 2)), (8, 0, 2, (8, 2))])
def test_mutation_timeouts_are_not_caught(
    tmp_path: Path, caught: int, missed: int, timeout: int, score: tuple[int, int],
) -> None:
    path = tmp_path / "outcomes.json"
    path.write_text(json.dumps({"end_time": "2026-10-18T00:00:00Z", "total_mutants": 10,
                               "caught": caught, "missed": missed, "timeout": timeout, "unviable": 0,
                               "outcomes": [{"scenario": "Baseline", "summary": "Success"}]}))
    assert load_tool("mutation_score").counts(path) == score


@pytest.mark.parametrize("report", [
    {"caught": 100},
    {"end_time": "done", "outcomes": [{"scenario": "Baseline", "summary": "Failure"}]},
    {"end_time": "done", "total_mutants": 100, "caught": 1,
     "outcomes": [{"scenario": "Baseline", "summary": "Success"}]},
])
def test_invalid_mutation_evidence_rejected(tmp_path: Path, report: dict[str, Any]) -> None:
    path = tmp_path / "outcomes.json"
    path.write_text(json.dumps(report))
    with pytest.raises(ValueError):
        load_tool("mutation_score").counts(path)


def test_fuzz_uses_measured_usage(tmp_path: Path) -> None:
    path = tmp_path / "time.txt"
    path.write_text("Command exited with nonzero status 1\n3.5 1.5 2.5\n")
    assert load_tool("fuzz_evidence").usage(path) == (5.0, 2.5)
    path.write_text("nan 0 0\n")
    with pytest.raises(ValueError):
        load_tool("fuzz_evidence").usage(path)
