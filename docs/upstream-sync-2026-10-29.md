# Upstream synchronization: 2026-10-29

Upstream: `22a6979374808dd7fedc38e9bbdfee1571f17391` (0.7.2).
Previous integrated upstream: `4309b11bf53dadf69887a3323506f29169e849bd`.
Previous downstream tip: `69946d26cdd924ee01877d16da4dcd50e2fb3f10`.

The 16 downstream commits were rebased on the 35 new upstream commits. The
configuration keeps display-local workspaces and leaves Rift Overview disabled.

## Removed or replaced with upstream code

| Downstream code | Replacement and verification |
| --- | --- |
| Built-in display detection and duplicate `ScreenInfo.is_builtin` plumbing | Upstream `System::is_builtin` and `ScreenCache`. `src/sys/screen.rs` now matches upstream exactly. Test fixtures retain their explicit built-in display identities. |
| Duplicate `WorkspaceStore::workspace_index` | Upstream implementation over `scope_ids`, which handles both workspace modes. |
| Rich menu-bar workspace snapshots and their model structs | Upstream `visible_workspace_view`, `menu_workspaces`, grouped `Update`, fullscreen flags, and render cache. The downstream border/palette snapshot no longer collects menu window data. Existing menu tests exercise the upstream update channel. |
| Obsolete `MoveWindowToDisplayPayload` and its unused handler | Upstream reactor display movement and the shared `request_window_transfer` boundary. |
| Manual native-space cache/visibility writes after affinity moves | Upstream `WindowStore::request_transfer` and `LayoutEngine::prepare_window_placement`. Logical affinity movement still uses a full window identity. |

## Retained because the behavior differs

| Downstream behavior | Why upstream does not replace it |
| --- | --- |
| Display-local workspace affinity | Upstream `workspace_rules.display` applies only in global mode. It does not preserve the existing per-display scratch workspace or `external_primary` selector. |
| Full `WindowId` affinity movement | The protocol's numeric window selector can select a different app's window on the destination in the existing regression fixture. `move_window_to_workspace_index_on_space` retains the exact identity and uses upstream relocation primitives. |
| Floating cross-display drag and fixed-axis size constraints | Upstream drag still queues permanent `WindowRemoved`; its removal still clears size constraints during a preserving removal. |
| Bounded menu updates | Upstream resets its 150 ms debounce on every update. The local timer starts only when the pending slot was empty. |
| AX identifier matching and batched identity reads | Required to distinguish Arc Little Arc windows; no equivalent upstream matcher. |
| Unmanaged and untiled focus history | Upstream has no equivalent reactor commands or combined floating/unmanaged MRU workflow. |
| Window-state border colors and command palette | These remain downstream UI features. The palette also supports upstream `Balance` and configured `Sequence` commands after this synchronization. |
| Reset all configured workspaces and scrolling widths | Upstream `balance` acts on the selected layout. It does not restore configured app placement across displays or reset all scrolling columns. |
| Dedicated border compositor connection | cgs 0.3.19 creates windows on its main connection. It does not replace the border's private connection, persistent context, motion handle, and freeze/redraw transaction. |

## Verification boundary

No downstream test added since the previous upstream base was dropped. Tests
which referenced removed menu snapshot types now assert the upstream menu data
and the retained border snapshot independently.

`named_pin_reload_moves_immediately_without_growing_catalog` fails at the same
native-move assertion in both the unmodified upstream source archive and the
rebased fork. The upstream full library run also fails
`empty_active_space_membership_during_wake_race_does_not_blank_known_active_windows`;
the fork retains the earlier fixture correction for that test. The global-pin
failure is recorded rather than suppressed or given another downstream fix.

The fork's complete library/binary test run has 1,020 passing tests and the one
global-pin failure above. `cargo clippy --locked --all-targets --all-features
--no-deps` completes with 308 existing library-test warnings; strict warning-free
Clippy is not claimed. After copying the current local configuration into its
repository snapshot, all nine command-catalog tests pass, including parsing that
exact profile. The local configuration file itself is unchanged.
