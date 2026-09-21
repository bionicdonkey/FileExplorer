# Accessibility acceptance

## Automated gate

Run on an X11/Xvfb display with the repository's GTK 4.12+ / libadwaita 1.5+ dependencies:

```sh
xvfb-run -a cargo test --test accessibility --test headless_gtk slice4_
GDK_SCALE=2 xvfb-run -a cargo test --test accessibility --test headless_gtk slice4_
xvfb-run -a cargo test --quiet
```

The accessibility suite checks explicit icon names, expansion state, Escape from location editing, selected-file keyboard menus and focus return, focus retention across sidebar refresh and folder navigation, and operation terminal announcements without repeated polling announcements. Existing Slice 3 tests cover selection, command dispatch, clipboard/drop ownership, properties focus, and permanent-delete Cancel/Confirm.

The rendered fixture loads production CSS and the real location/header controls. It rejects CSS parse errors and horizontal minimum sizes exceeding the requested window width, renders each case with GSK, and verifies GTK widget direction. Cases cover 1000-pixel and 560-pixel windows, Sans 11 and Sans 22, the application's high-contrast CSS class, RTL, and disabled GTK animations. An additional process with `GDK_SCALE=2` exercises 200% display scaling. The fixture is a component layout check, not a screenshot comparison of the entire application or a numeric WCAG contrast audit.

To save the rendered fixture images for inspection:

```sh
EXPLORER_A11Y_SNAPSHOT_DIR=/tmp/explorer-accessibility-fixtures xvfb-run -a cargo test --test headless_gtk slice4_
```

Inspect text, current-location borders, focus outlines, mirrored navigation order, clipped content, and long breadcrumb scrolling. Native GTK theme colors supply foreground/background contrast; high-contrast rules remove dimming and use solid theme-foreground outlines. Real desktop high-contrast preference propagation must also be checked manually.

## Keyboard workflow

Use a disposable local folder containing two files and a subfolder. Keep authentication and permission prompts under GIO/GTK ownership.

1. Use Tab and Shift+Tab to traverse title-bar controls, navigation, location, file actions, search/view controls, sidebar and file content. Use Ctrl+L to edit the location. Enter navigates; Escape restores the committed location and returns to breadcrumbs. Invalid input stays in the editor with an accessible invalid state and explanation.
2. Use Ctrl+F to focus search, enter a term, then clear it to return to the folder. Use F6 to move between the sidebar and file pane. Use arrow keys and Enter to open a folder; focus must stay in the new listing. Alt+Left/Right navigate history, Alt+Up opens the parent, and F5 refreshes.
3. Select files with the native list/grid keyboard controls. Shift+F10 or Menu opens the selection's context menu. Traverse with Tab/Shift+Tab, activate with Enter/Space, and dismiss with Escape. Focus must return to the invoking control and selection must remain intact. Repeat on a sidebar location and a tab.
4. Use Ctrl+C/Ctrl+X and Ctrl+V, or the keyboard menu, to copy/move a test file. F2 opens Rename. Delete uses Trash; Shift+Delete opens permanent-delete confirmation with Cancel as its default. Cancel first, then confirm only a disposable test file. Unavailable actions expose their reason as an accessible description.
5. Use Ctrl+T, Ctrl+PageUp/PageDown and Ctrl+W for tabs. The existing last-file-tab rule still applies. Open Properties from the context menu, inspect the named fields, and close it; focus must return to its parent. Large metadata values wrap and scroll.
6. Open File operations. Progress has a native progress-bar value and value text, and each Cancel control is named “Cancel operation” with its operation/item context. Cancel an active operation and inspect its terminal outcome. Completed items and unknown/partial effects must remain clear.

## Orca and platform checklist (manual)

On a real GNOME/Zorin session, enable Orca using the desktop accessibility controls. Repeat the keyboard workflow with speech enabled and without a pointer. Verify names, role, current location, expanded/collapsed state, selection, unavailable-action descriptions, error messages and confirmation defaults. Pay particular attention to nested context menus and focus after asynchronous refreshes.

Status policy: the window contains stable GTK `Status` and `Alert` labels. `AccessibleStatus::announce` suppresses consecutive identical messages. Ordinary navigation/search outcomes and operation start/completion use Status; errors, destructive confirmation and disconnection use Alert. The operation watcher announces state boundaries, never byte-count polling. A fast operation may report start and completion together; existing terminal history is silent when a window opens. GTK 4.12 is retained, so this does not use the newer `gtk_accessible_announce` API. Confirm actual Orca delivery/order on supported GTK/AT-SPI versions; inspecting roles in a test does not establish speech delivery.

Repeat at 100% and 200% display scale, normal and large desktop text, light/dark and high-contrast desktop themes, a 560-pixel-wide window, reduced animation, and an RTL locale. Use the sidebar toggle when more file-pane width is needed. Check all file view modes and a long filename/URI. GTK handles native theme, text, tab traversal and animation settings; no custom motion is required to understand state.

For a permitted disposable remote folder, test connect, cancelled authentication, disconnection, reconnect and unmount failure. Check that native credential prompts remain accessible and no credentials appear in labels or status. Do not use production files for mutation acceptance. These real screen-reader, desktop-theme, Wayland and live-provider checks are not replaced by the Xvfb fixture and must be recorded separately.
