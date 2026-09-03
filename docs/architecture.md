# Rift architecture

## State and presentation boundary

Rift treats the reactor as the owner of desktop domain state. macOS events are
reduced synchronously, all follow-up effects are applied, and only then is one
immutable `DesktopSnapshot` committed for presentation consumers.

Every committed snapshot has a monotonically increasing `StateRevision`.
Equivalent states do not advance the revision or notify consumers. Presentation
actors must ignore stale revisions and render idempotently.

The boundary has three invariants:

1. Presentation actors never query mutable reactor state.
2. Presentation actors never create a second AX or SkyLight observation model.
3. A domain transaction publishes at most one committed snapshot after all
   nested state transitions have completed.

On multi-display systems, focused-window identity comes from WindowServer's
key-focus process queried across all visible native spaces. A native focus
result also updates the globally frontmost process. `CGSGetActiveSpace` is not a
focus authority because it represents only one display context.

## Migration status

The menu bar and focused-window border are migrated consumers. The menu bar's
previous event-specific update calls and lossy view signature have been replaced
by the shared snapshot stream. The border derives focused-window identity,
visibility, geometry, and Mission Control suppression from the same committed
state, and does not maintain a second AX or SkyLight observation model.

The border uses one transparent buffered WindowServer window and a persistent
CGContext on a dedicated SkyLight connection. Its WindowServer shape is a
rounded ring rather than a full rectangle. The overlay carries the floating and
ignore-for-events tags, lives on the target space, and inherits the target
level/sublevel. Rift queries the resulting tags before making the surface
visible. Tag setter failures prevent the surface from becoming visible.
Read-back is diagnostic because a new private-connection window can temporarily
return stale tags; the hollow shape independently keeps the target content
outside the overlay's event footprint.

After initial placement, Rift adds the overlay to the target's movement and
ordering groups. A verified movement group lets WindowServer carry the border
in the same compositor operation as an interactive target-window drag, so
position-only snapshots update model state without replaying stale frame moves.
An ordering group carries ordinary order-in/out changes. Group attachment is a
private-SPI optimization, not a correctness requirement: failed or ineffective
attachments fall back to coalesced frame and relative-order synchronization.
Changing the target WindowServer id creates a replacement surface instead of
trying to detach and reuse a child across groups.

A size or style change disables connection updates, freezes the surface,
reshapes and redraws it, thaws it, and only then restores visibility. Activating
click and ordering-group notifications bypass snapshot deduplication and ask the
border actor to repeat the relative-order transaction without mutating desktop
domain state. This keeps presentation-only z-order invalidation separate from
the committed desktop snapshot.

The remaining migration is deliberately incremental:

1. Derive StackLine state from `DesktopSnapshot`.
2. Derive IPC events and query projections from the same committed revision.
3. Consolidate workspace, display, and focus ownership into the reactor's domain
   state rather than reconstructing them from several managers.

Until a consumer is migrated, its existing transport remains in place. New
presentation features must use the snapshot boundary rather than adding another
direct channel from an event handler.

## Command palette

The command palette is a lightweight presentation path independent of Mission
Control. When summoned, its actor requests one immutable `PaletteSnapshot` from
the reactor. That snapshot includes tracked managed and unmanaged windows,
running applications, workspace/display labels, the authoritative frontmost
focus, and a fixed whitelist of safe Rift commands. Desktop snapshots update
the in-memory MRU while the panel is hidden; there is no polling or persisted
history.

The pure model owns the complete ranked result set, fuzzy matching, stable
selection identity, explicit window/command scopes, and application drill-down. The main-thread UI owns only a
prewarmed borderless `NSPanel`, native AppKit input/scrolling, and rendering
state. `max_results` controls the visible viewport rather than truncating model
data; keyboard selection scrolls into view and drawing is limited to dirty rows.
The panel uses a cold HUD surface, a custom non-interactive scroll indicator,
and a dynamically sized viewport; those presentation choices remain isolated
from ranking and actions.
Printable and IME input stays inside AppKit while navigation becomes typed actor
events. The panel orders out before dispatching an action or restoring captured
focus, and it never queries AX, WindowServer, or mutable reactor state directly.
