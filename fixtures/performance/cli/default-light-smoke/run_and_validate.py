#!/usr/bin/env python3
"""Launch the bounded flow driver; the authority judges its exact evidence after exit.

The host uses perflab.dev_flow for both local files and supervisor-transferred bytes.
The stdout summary is never the correctness oracle.
"""
import os
from pathlib import Path
import sys


def main():
    os.environ["PYTHONDONTWRITEBYTECODE"] = "1"
    driver = Path(__file__).resolve().with_name("driver.py")
    os.execv(sys.executable, [sys.executable, "-I", "-B", str(driver), *sys.argv[1:]])


if __name__ == "__main__":
    main()
