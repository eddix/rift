# Upstream synchronization: 2026-11-12

Upstream: `6101a78a4958e942853eb21cb67f8012fb293458` (0.8.1).
Previous integrated upstream: `ab131085bbdd378f48bd26067a8fcd46fb2d2446`.
Previous downstream tip: `28824ec7dc06ffc7c4e52591d14db7abf5e4e446`.

The 22 downstream commits were rebased on 39 new upstream commits. The personal
profile now uses upstream global workspace ownership and display placement.

## Global personal profile

- One catalog contains five workspaces. Workspace 1 prefers `builtin`;
  workspaces 2–5 prefer `external`, with `builtin` as fallback. Workspace 5 is
  one global Misc workspace, rather than one scratch workspace per display.
- Placement uses `workspace_rules.display`. The personal profile no longer
  uses the downstream `workspace_display_rules` policy. That legacy option is
  retained for existing per-display configurations.
- The upstream preference resolver gains one portable selector, `external`.
  It chooses the leftmost eligible non-built-in display, then its vertical
  position and UUID break ties. The settings editor recognizes this selector.
  Ownership changes, manual overrides, departure and restoration continue to
  use the upstream implementation.
- The Agent workspace retains scrolling layout. Existing application rules,
  Overview, shortcuts, palette and border settings are retained. Redundant
  dialog/sheet floating rules are removed: upstream now rejects these transient
  windows before applying a management override.

Switching an existing per-display catalog to global mode retains its workspace
identities and disambiguates duplicate names. It does not merge two five-entry
catalogs. Runtime migration therefore starts with a fresh global catalog and
reconciles surviving windows against the saved per-workspace inventory. The
old configuration, binaries, source and complete layout snapshot are backed up
before activation. An old ten-workspace snapshot must not be restored over the
new five-workspace catalog.

## Lifecycle integration

Upstream suspension preserves canonical tree membership while temporarily
excluding windows from geometry. Local eager border invalidation uses the
combined visibility handler; focus, palette, reset and affinity retain the
ownership-aware admission boundary. Scrolling rebalance passes the upstream
inactive set into layout mutation. The cross-display fixed-axis regression now
updates logical ownership before `WindowAdded`, matching the new lifecycle.

App-rule validation uses upstream complete-matcher deduplication with the local
`ax_identifier_regex` included in its key. Reusing an identifier expression
between different apps remains valid; an exact duplicate remains rejected.

The upstream animation regression still expected a newer revision alone to
cancel presentation, although `ccd12e8` changed that policy to actual presentation
changes. The fixture now checks that newer membership-only revisions preserve
animation and a changed screen frame cancels it. Production cancellation policy
is unchanged.

## Verification

- Locked workspace library and binary tests: **1,057 passed**, no failures or
  ignored tests, with the kache test runner unset for this command.
- The personal-profile regression covers five unique identities, external Misc
  placement, moving a window into Misc, unplug fallback and reconnect restoration.
- Upstream suspension/fullscreen, claims, native-width, drag and Overview tests
  pass alongside all retained downstream feature tests.
- All-target, all-feature workspace Clippy completes with existing warnings.
  Warning-free Clippy is not claimed.
- Nightly formatting and diff whitespace checks pass.

The canonical profile remains the
[configuration Gist](https://gist.github.com/eddix/d1ccc24ec3af583b6842ce3ac99d57d3);
`config/eddix.toml` is its repository mirror.
