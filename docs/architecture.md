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

The border keeps the CGS backing resolution at 1x because the shared WindowServer
context renderer clears in logical-point coordinates. HiDPI changes only the
`CALayer` contents scale; changing the CGS backing scale without also changing
the CGContext clear/render transform can leave uninitialized opaque pixels over
the target window.

The remaining migration is deliberately incremental:

1. Derive StackLine state from `DesktopSnapshot`.
2. Derive IPC events and query projections from the same committed revision.
3. Consolidate workspace, display, and focus ownership into the reactor's domain
   state rather than reconstructing them from several managers.

Until a consumer is migrated, its existing transport remains in place. New
presentation features must use the snapshot boundary rather than adding another
direct channel from an event handler.
