Every GitHub pull request now defaults to functional, e2e and uix validation, including AF gate
and release PRs. `make release` and `scripts/release.sh` default to fast; explicit class labels
and `release:full` retain their precedence. An unlabeled matching merged release PR keeps its
fast selection in the immutable release tag binding. Local `make check`, ordinary main and
unbound releases remain full. Version 2 plan, receipt and release-binding schemas record the
new release PR context; earlier versions fail closed. Base gates and packaged-release smoke
remain mandatory, and excluded perf work is reported as NOT RUN or exempt.
