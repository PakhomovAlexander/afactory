#!/usr/bin/env bash
# Recolour the repository's issue labels onto the brand palette. Idempotent (`--force`
# updates an existing label in place). Run by a human with `gh` logged in:
#
#   brand/github-labels.sh PakhomovAlexander/afactory
#
# The palette (brand/README.md, "GitHub"): pink = something is wrong, blue = something to
# do, green = something settled or welcoming, grey = housekeeping, ink = tooling.
set -euo pipefail
repo="${1:?usage: github-labels.sh OWNER/REPO}"

label() { gh label create "$1" --repo "$repo" --color "$2" --description "$3" --force; }

label bug              EE366A "Something isn't working"
label invalid          EE366A "This doesn't seem right"
label enhancement      5195F5 "New feature or request"
label question         5195F5 "Further information is requested"
label documentation    5195F5 "Improvements or additions to documentation"
label "help wanted"    36EEA8 "Extra attention is needed"
label "good first issue" 36EEA8 "Good for newcomers"
label accessibility    36EEA8 "Barrier affecting people with disabilities"
label duplicate        999999 "This issue or pull request already exists"
label wontfix          999999 "This will not be worked on"
label dependencies     0F0F0F "Pull requests that update a dependency file"
label github_actions   0F0F0F "Pull requests that update GitHub Actions code"
label rust             0F0F0F "Pull requests that update rust code"
