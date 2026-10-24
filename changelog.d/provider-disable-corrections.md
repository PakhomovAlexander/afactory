Provider disable now preserves long registered IDs within the existing 64 KiB aggregate state
bound, and `af provider enable ID` can clear a retained disable entry after registry removal to
recover the 32-entry capacity. Unknown IDs and unsafe or malformed state still fail closed;
updates remain private, atomic and token-free. Command, status and refusal diagnostics show the
exact expiry as a UTC calendar date and time while persisted Unix seconds and nanoseconds stay
unchanged. Provider documentation now correctly describes `distinct_providers` as comparing
adapter families, preserving the existing trusted principal/model policy knobs.
