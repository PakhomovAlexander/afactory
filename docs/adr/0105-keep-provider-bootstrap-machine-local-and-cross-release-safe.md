# ADR-0105: Keep Provider bootstrap machine-local and cross-release safe

**Status:** accepted (2026-09-15)

Provider authentication and its registry are machine-local bootstrap state, not repository
authority. `af provider setup` and `af provider recover` therefore run in the invoking release and
do not dispatch through a repository's `.af/af.lock`. This narrowly revises ADR-0044's exhaustive
dispatch list; all other Provider commands and every Review or Task execution retain their existing
dispatch behavior.

The machine-local Provider registry remains version 1 and must be safe while supported pinned
releases read it. A registry replacement is prepared and validated completely before one atomic
filesystem exchange. That exchange is the irreversible cross-release commit point: an older reader
that does not understand the newer transaction marker can observe only the complete old registry or
the complete committed candidate. Publication never rolls the candidate back after the exchange.
Recovery copies remain durable, and failures in post-commit sync, marker archival, or recovery
maintenance are warnings rather than claims that the already-visible update failed. A later
non-cooperating replacement is reported separately and never destroys the preserved candidate.

Interactive setup is serialized by Provider kind and canonical auth-directory identity, independent
of `AF_PROVIDERS_FILE`, so two registries cannot drive one credential context concurrently. Explicit
registration rejects symlinked, foreign-owned, or group/world-writable auth directories on Unix,
and admission revalidates the directory and its rename-controlling ancestor chain before paid
Provider work. Directory handles remain open across auth locks, reads, and publication so a path
replacement is detected. Registry publication creates private directories and `0600` files
independent of the caller's umask, and rejects registry state that another local user can replace or
mutate. Auth paths that cannot be represented in the TOML registry are rejected before directory
creation, probes, or interactive login.

An unfinished transaction fails closed. `af provider recover` parses only the closed marker shape,
confines every referenced file to its recorded recovery directory, and validates the live registry,
candidate copy, and prior copy against the marker's hashes. Only then does it classify the live
version and atomically archive the marker. It never selects or copies registry bytes and never
deletes either preserved version.

## Considered options

- **Dispatch setup or recovery through the project pin.** Rejected because an older release cannot
  bootstrap a command it does not contain, and machine credentials are not project authority.
- **Use the registry pathname as the login lock.** Rejected because the registry path is
  configurable while the provider auth context is the shared side effect.
- **Depend on the transaction marker to hide a tentative candidate.** Rejected because supported
  older readers do not know that marker. The live pathname may contain only an irrevocably committed
  valid registry.
- **Keep setup machine-local, lock the auth context, and make atomic exchange the commit point
  (chosen).** This gives one-command bootstrap without weakening pinned execution or cross-release
  reads.

## Consequences

- A user can run the installed `af provider setup` or `af provider recover` from any repository,
  including one pinned to an older release.
- Old and new readers may overlap publication safely; newer readers additionally fail closed on an
  unfinished recovery marker and provide a hash-validating recovery command.
- Once atomic exchange succeeds, cleanup failures cannot make the command claim that publication
  itself failed.
- Auth-directory safety is enforced both when an explicit binding is written and when it is used.
