# ADR-0239: Open the browser before any Store is read

**Status:** accepted (2026-10-27). Refines the read-first browser of
[ADR-0119](0119-open-a-read-first-browser-on-bare-af.md) and the Store registry listing of
[ADR-0144](0144-hold-afs-disk-use-to-a-machine-budget.md); issue #402.

## Context

Bare `af` paints its splash a few milliseconds after it starts and waits on one thread that
resolves the scope and loads every pane. Three panes read Stores inside that load, on that
thread: the Tasks pane read every target (the scope's own Store, every Task Store the registry
`stores.toml` records and, outside a repository, every repository's default Store), each through
`task_execution::list_common`, which replays every Task stream of the Store and validates its
artifacts; the Campaigns pane read every Campaign source the same way; the Workers pane, once
opened, read `HEAD` through git and every Task Store's Attempts on the key loop.

On the maintainer's machine the registry named 81 Stores and the state root held 166 event logs,
4.0 GB in all. Measured on 27 October 2026 with af 0.13.0 in a pseudo-terminal, the browser's
first frame came 40.6 s after launch at a load average of 60 to 110, and not within 45 s at a
load of 125; with an empty private HOME it came in 0.13 to 0.33 s. `af task list` in the same
repository answered in 0.07 s. A splash that moves for forty seconds reads as an infinite load.
The design promised the opposite: "the browser is never later than it would be without one".
The Providers pane already did it right, discovering on a thread behind a spinner.

## Decision

The load the splash waits for resolves the scope and loads the panes; it reads no Store. A pane
that lists Stores starts their read on a thread of its own in `load`, shows a spinner on its
folder while that read runs (`Pane::busy`), and collects the result in `poll`. The event loop
paints before it polls, so the browser's first frame never shows, and never waits on, a Store:
the order is the loop's, not a race. What a load still does on the calling thread is small and
bounded: it resolves the scope's targets (the Store registry `stores.toml` is one file, read and
its paths canonicalised), reads `.af/` and the layer directories, and, in the Pipelines pane,
reads `HEAD` through git as before this change, milliseconds on a local repository.

The Tasks and Campaigns panes deliver one message per Store (per source), in target order, and
list each the moment it arrives: the folder fills while the rest are still read, a refused Store
shows its refusal when it fails, and the last message ends the read and the spinner. The opened
Task's detail travels with its own Store's message, so its bar row and its pane come from one
read. The Workers pane reads `HEAD` and the Stores on a thread once opened, behind the same
spinner. `R`, `:state DIR`, a scope reload and the reread after a handed-off command take the
same path; a live reread of an opened running Task is the same read started on its cadence.
Closing a pane or opening another entry drops a live read, never a load's. Nothing interrupts a
read: it holds no key, `Esc` and `<C-c>` map to one action and a habitual `Esc` must not leave the
folder half filled, and `q` quits without waiting for it, as it always did for a live read.

Three `uix` tests assert the order on the pseudo-terminal's byte stream: the `tasks/` folder
with its spinner is painted before any Store, with eight registered Stores in the project scope
and eight repositories' Stores in the user scope, and a refused Store registered first shows its
refusal before the seven Stores after it. Each waits for the terminal state (every Store listed,
the spinner down, the status line back) before it reads the bytes, since the last Store's message
and the end of the read are two messages. The spinner assertion is what the tree before this
change fails. Unit tests prove a Store listed while another is unread, and the spinner up until
the read's last message, without timing, by feeding the pane's channel.

An opened entry that a read in the background no longer lists falls back to its folder with the
same message as when opening finds it gone (`App::poll`), since a read that used to answer
inside `open` now answers later: a Worker whose declaration went between the listing and the
read, or a Task whose Store refused. A refusal taken in `open` (the opened Task can no longer be
inspected) restarts a read in flight, so that a Store message read before the refusal never lists
the Task again over it.

## Consequences

- The browser's first frame no longer depends on the number or size of the Stores a machine
  holds, which is what the performance target of the first frame (the laboratory case `tui_open`,
  a separate change) can demonstrate.
- Reading a Store still costs its full replay and artifact validation, on every reread of an
  opened running Task as well: the listing's integrity rule stands, and its cost is
  [issue #380](https://github.com/PakhomovAlexander/afactory/issues/380)'s subject, not this one's.
  Opening one Task still inspects it on the key loop (`Detail::read`, one `af task show`), as
  before: one Task, not a listing.
- The Pipelines pane still reads `HEAD` through git inside `load` and on `R`, synchronously, as
  before this change. A repository on a slow or unreachable filesystem can hold the splash on
  that read; moving it behind the browser keeps the preview's exact-commit binding and is a
  change of its own, not this one.
- A pane's `items()` lists what has arrived: a test that loads a pane and reads its items at once
  sees nothing, and settles the read first (`settle`, the tests' event loop).
- The launch-to-browser numbers above were measured by the owner's session in a pseudo-terminal
  (twenty registered Stores, a private HOME, five runs each) and are not a laboratory receipt;
  the laboratory case that makes the first frame a performance target is Task B of the campaign.
