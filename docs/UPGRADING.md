# Upgrading

Notes for people arriving from an older release. Nothing here applies to a
fresh install.

## From v0.x

v1.0 was a complete rewrite. Settings, the config format and the internals are
all different.

1. Remove the old `addons/godot_vim/` folder from your project before
   installing the new one.
2. Old GodotVim keys in your editor settings are harmless and ignored, but you
   can delete every line starting with `plugins/GodotVim` for a clean slate.
   The file is:
   - Windows: `%APPDATA%\Godot\editor_settings-4.tres`
   - Linux: `~/.config/godot/editor_settings-4.tres`
   - macOS: `~/Library/Application Support/Godot/editor_settings-4.tres`
3. Recreate your key mappings. v0.x stored them in editor settings; v1.x reads
   them from a `.godot-vimrc` file in your project. `:mkvimrc` writes a
   starter with every preset listed and commented out.

## From v1.6.x

With no `.godot-vimrc`, the keyset is unchanged. The keys outside the text
buffer (panel focus, dock navigation, FileSystem and debugger operations, the
completion popup) became a rebindable table in v1.7.0; the shipped defaults are
what they were. See [Panel key bindings](REFERENCE.md#panel-key-bindings-panelmap).

One difference you may notice: `Ctrl+Enter` no longer confirms a completion.
The old popup handler matched `Enter`, `Tab`, `Escape`, `Up` and `Down` while
ignoring modifiers, so every modified variant was swallowed too. `<CR>` now
means `<CR>`, and modified variants reach the Vim engine. To restore the old
`Shift+Up` behaviour:

```vim
panelmap <shift> editor.completion <Up> godotvim.completion.navigate
```
