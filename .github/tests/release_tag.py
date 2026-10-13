"""Run the actual publication step with a mocked GitHub API and no release writes."""

import os
from pathlib import Path
import subprocess
import tempfile
import textwrap

workflow = (Path(__file__).parents[1] / "workflows/release.yml").read_text()
publication = workflow.split("      - name: Verify and publish archives\n", 1)[1]
script = textwrap.dedent(publication.split("        run: |\n", 1)[1])
expected = "a" * 40

for name, actual, api_status, should_publish in [
    ("matching tag", expected, 0, True),
    ("moved tag", "b" * 40, 0, False),
    ("missing tag or API failure", "", 1, False),
    ("empty API response", "", 0, False),
]:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        (root / "dist").mkdir()
        (root / "dist/archive.tar.gz").write_bytes(b"fixture")
        (root / "dist/archive.sha256").write_text("0" * 64 + "  archive.tar.gz\n")
        gh = root / "gh"
        gh.write_text('''#!/usr/bin/env python3
import os, pathlib, sys
if sys.argv[1] == 'api':
    assert sys.argv[2:] == ['repos/owner/repo/commits/refs/tags/v0.1.0', '--jq', '.sha']
    if int(os.environ['MOCK_API_STATUS']):
        sys.exit(int(os.environ['MOCK_API_STATUS']))
    print(os.environ['MOCK_TAG_SHA'])
else:
    assert sys.argv[1:3] == ['release', 'create']
    pathlib.Path('published').touch()
''')
        gh.chmod(0o755)
        checksum = root / "sha256sum"
        checksum.write_text("#!/bin/sh\nexit 0\n")
        checksum.chmod(0o755)
        result = subprocess.run(
            ["bash", "-e", "-o", "pipefail", "-c", script],
            cwd=root,
            env={**os.environ, "PATH": directory + os.pathsep + os.environ["PATH"],
                 "GH_REPO": "owner/repo", "RELEASE_TAG": "v0.1.0",
                 "GITHUB_SHA": expected, "MOCK_TAG_SHA": actual,
                 "MOCK_API_STATUS": str(api_status)},
            capture_output=True, text=True,
        )
        assert (result.returncode == 0) == should_publish, (name, result.stderr)
        assert (root / "dist/published").exists() == should_publish, name
        print(name + ": passed")
