Repair macOS performance-laboratory fixture setup by canonicalizing newly created temporary
roots, using explicit Python no-op executables, modeling the Linux mount host explicitly and
exercising log preallocation with real dense allocation where the platform lacks
`posix_fallocate`. Preserve strict symlink refusal, allocation-before-launch, log overflow,
absolute deadlines and cleanup ownership assertions; production behavior and test-class
selection remain unchanged. Anchor the terminal stall fixture’s simulated clock jump to its
recorded start so setup duration cannot consume its declared boundary interval twice.
