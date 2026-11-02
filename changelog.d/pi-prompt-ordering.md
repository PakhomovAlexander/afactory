- Pi setup now emits `af/pi-provider-setup@2` for all internal refusals, with safe stderr
  details and null identity/backend fields for invalid requests. Pi host protocol v2 requires
  a fresh delivered-prompt challenge before accepting inference results; immediate valid
  replies remain supported. Managed SDK discovery validates malformed versions in process.
  See [ADR-0274](https://github.com/PakhomovAlexander/afactory/blob/main/docs/adr/0274-own-the-pi-sdk-host-and-confine-its-tools.md).
