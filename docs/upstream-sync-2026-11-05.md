# Upstream synchronization: 2026-11-05

Upstream: `ab131085bbdd378f48bd26067a8fcd46fb2d2446`.
Previous integrated upstream: `22a6979374808dd7fedc38e9bbdfee1571f17391`.
Previous downstream tip: `70d338509cda041ae45bceda875e939cabf7f3cf`.

This incorporates 19 upstream commits. Seven arrived after the initial audit,
including the persistence rewrite, stale recovery-process cleanup, and fixes
for inactive workspace parking and scrolling widths after display moves.

## Shared upstream implementations

- The command palette now discovers built-ins and labels through the upstream
  `CommandCatalog` derive on `LayoutCommand`, `ReactorCommand`, and `WmCmd`.
  The duplicate inventory of unit commands and default parameters is removed.
  A local adapter expands runtime workspace/display targets and finite argument
  choices. Incomplete editor templates such as empty exec commands, placeholder
  window IDs, and empty file paths are excluded from executable defaults.
- Local command metadata participates in the native shortcuts editor. Palette
  toggle aliases are cataloged through their canonical `WmCmd` form so TOML
  round-trips preserve the command variant and duplicate rows are avoided.
- The previous wake-test fixture correction is removed. Upstream #640 keeps
  unit tests from querying real desktop window lists. Both formerly failing
  window-list tests now pass without a local workaround or a skipped test.
- Downstream UI imports use the reorganized cgs 0.5.1 modules.
- Persistence, master-layout restoration, and stale recovery-process handling
  use the new upstream subsystem. `RestoreScope::Displays` is available in the
  palette alongside the existing scoped restore commands.

## Preserved behavior and compatibility

The display-local affinity policy, combined floating/unmanaged focus history,
AX identifier matcher, focused-window border colors, palette UI, bounded menu
refresh timer, and reset-all-workspaces behavior remain downstream features.
Their replacement boundaries are documented in the earlier synchronization
audit. Cross-display floating classification and preserved size constraints
still require the local drag/removal fixes.

External ownership adds an important admission distinction: a claimed window
can pass ordinary `WindowState` heuristics while Rift must leave it unmanaged.
Downstream focus, affinity, reset, drag, and border checks use the ownership-aware
`WindowStore` admission API. Explicit focus can include a visible claimed window
in the untiled group, but it emits no layout focus event or frame write. Its
focused border uses the unmanaged color. A regression test covers this boundary.

The private border compositor connection and exact `WindowId` affinity move
adapter remain necessary; the new SDK ownership API is a separate interface.

## Verification

- Complete locked workspace library and binary tests: **1,023 passed**, no
  failures or skipped tests, using one test thread and the per-command kache
  runner workaround.
- The previous `named_pin_reload_moves_immediately_without_growing_catalog`
  and wake-membership failures both pass with upstream #640.
- Native shortcuts metadata, TOML round-trips, configured command sequences,
  generated palette coverage, unresolved-template filtering, and external-claim
  focus/border behavior are included in the passing suite.
- All-target check, nightly formatting, and diff whitespace checks pass.
- Ordinary all-target/all-feature Clippy completes with existing warnings;
  strict warning-free Clippy is not claimed.

The personal configuration remains display-local and keeps its current Overview
and shortcut settings. Its canonical synchronization target is the
[configuration Gist](https://gist.github.com/eddix/d1ccc24ec3af583b6842ce3ac99d57d3);
`config/eddix.toml` remains a byte-for-byte repository mirror.
