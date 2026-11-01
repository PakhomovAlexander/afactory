- A termination signal that a performance shard consumes during its final slot now ends the
  shard `interrupted` (`cancelled`), not `complete`. Before, the shard checked the signal only
  before the next slot, so a final sample that returned normally sealed a complete shard inside
  an interrupted original. Every completed sample is kept and none is counted missing. A stop
  the slot already made, such as an uncertain cleanup, is kept. Under a controller the
  cancellation still stops every later shard and original.
- Local core submissions now admit each side's native library workload before any claim or
  build. The native probe adapter registry is `af.perf-adapters/2`: each adapter declares the
  first AF release it covers (`min_version`, 0.12.0-rc.3 for the public-API probe). A release no
  adapter declares (v0.11.9), or a plan contract no declared adapter serves, is refused even
  for a library-only plan. `/1` is refused.
- CLI admission now checks every selected process case against the side's chosen adapter. A
  command the adapter does not declare, or declares pending, is refused with its reason: for
  example, `tui_open` on 0.12.0-rc.3 under `af_cli_v1`. The adapter's runnable commands stay
  admitted.
