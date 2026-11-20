"""Run the pinned Microsoft RDL baselines with a locally built ripbi binary."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
from urllib.request import urlopen

REVISION = "acc2ee0d1884765e4b5213149430fb063d166719"
SOURCE = f"https://raw.githubusercontent.com/microsoft/Reporting-Services/{REVISION}/PaginatedReportSamples"
# Report name, SHA-256, declared objects, unused objects.
SAMPLES = {
    "CountrySalesPerformance": ("2af53ac0aaad8d7078e7e05d2f2e3679fbf3010c308526368ad1a67eb13dc6a4", 12, 0),
    "Invoice": ("6251f6b9f76618dd5c2f9accc614b9e198fc221d2310a39508f6ac4897d53fdc", 50, 11),
    "Labels": ("d6f2f0580357efe676dd3b490ed982d5fbddd0a99e059e24fb8df10e014cbd6c", 14, 1),
    "Letter": ("65c0a4c835954f1756e435a217388276a8cf52ed2ee96763de3ddaa6a0a24815", 27, 4),
    "OrganizationExpenditures": ("22a41b7a8f504795ac68c4dad094d6f9bb7aa2d95cd0a3167c457e5d2cbe43f5", 19, 3),
    "RegionalSales": ("f5698ddeb06df3267322a34db6749f4e52b1e56aa0b6ad7d06a7713af768ce69", 14, 0),
    "Transcript": ("9693231c79853b0881d0414f1c98242c76216efc00784b3bc81acc69430b2e81", 12, 3),
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/ripbi"))
    parser.add_argument("--sample-dir", type=Path, help="use an existing offline copy")
    args = parser.parse_args()
    binary = args.binary.resolve()
    with tempfile.TemporaryDirectory() as directory:
        for name, (digest, objects, unused) in SAMPLES.items():
            filename = name + ".rdl"
            if args.sample_dir:
                content = (args.sample_dir / filename).read_bytes()
            else:
                with urlopen(f"{SOURCE}/{filename}", timeout=60) as response:
                    content = response.read()
            assert hashlib.sha256(content).hexdigest() == digest, filename
            Path(directory, filename).write_bytes(content)
            result = subprocess.run([binary, "scan", filename, "--json"], cwd=directory,
                                    capture_output=True, text=True)
            assert result.returncode == int(unused > 0), (name, result.stderr)
            summary = json.loads(result.stdout)["summary"]
            assert (summary["objects"], summary["unused"], summary["broken"]) == (
                objects, unused, 0), (name, summary)
            print(f"{filename}: {objects} objects, {unused} unused")


if __name__ == "__main__":
    main()
