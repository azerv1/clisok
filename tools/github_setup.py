"""Apply the solo-maintainer CI gate and protect release tags. Run explicitly via gh auth."""

import json
import shutil
import subprocess
from pathlib import Path
from typing import Any


def api(*args: str, payload: dict[str, Any] | None = None) -> Any:
    gh = shutil.which("gh")
    if gh is None:
        raise RuntimeError("gh is required")
    command = [gh, "api", *args]
    if payload is not None:
        command += ["--input", "-"]
    result = subprocess.run(command, input=json.dumps(payload) if payload is not None else None,
                            capture_output=True, text=True, check=True)
    return json.loads(result.stdout) if result.stdout.strip() else None


def main() -> None:
    gh = shutil.which("gh")
    if gh is None:
        raise RuntimeError("gh is required")
    repo = subprocess.run([gh, "repo", "view", "--json", "nameWithOwner", "--jq", ".nameWithOwner"],
                          capture_output=True, text=True, check=True).stdout.strip()
    protection = json.loads(Path(".github/branch-protection.json").read_text(encoding="utf-8"))
    api("-X", "PUT", f"repos/{repo}/branches/main/protection", payload=protection)
    name = "release tags"
    ruleset = {
        "name": name, "target": "tag", "enforcement": "active",
        "conditions": {"ref_name": {"include": ["refs/tags/[0-9]*", "refs/tags/v[0-9]*"], "exclude": []}},
        "rules": [{"type": "creation"}, {"type": "update"}, {"type": "deletion"}],
        "bypass_actors": [{"actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always"}],
    }
    existing = [r for r in api(f"repos/{repo}/rulesets") if r["name"] == name]
    endpoint = f"repos/{repo}/rulesets" + (f"/{existing[0]['id']}" if existing else "")
    api("-X", "PUT" if existing else "POST", endpoint, payload=ruleset)
    api("-X", "PUT", f"repos/{repo}/private-vulnerability-reporting")
    print("Configured ci-ok, protected release tags and private vulnerability reporting; no reviewer required.")


if __name__ == "__main__":
    main()
