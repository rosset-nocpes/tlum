# <img src="assets/icon.svg" width="64" height="64" align="middle" alt=""> &nbsp; tlum

macOS's **Look Up**, for Linux. Force-click any word on screen to get its definition, synonyms
and Wikipedia summary in a popover. Works on KDE Plasma, GNOME, sway and other wlroots
compositors, Hyprland, niri, and X11.

<p>
  <img src="assets/popover-dark.png" width="49%" alt="The popover in dark mode: dictionary entry, thesaurus and Wikipedia under a word marked in yellow">
  <img src="assets/popover-light.png" width="49%" alt="The same popover in light mode">
</p>

## Triggers

| Device | Gesture |
| --- | --- |
| Touchpad | Press the pad down and hold still (~0.6 s), or tap with three fingers |
| Mouse | Hold the left button still (~0.6 s), or a side button via `mouse_button` |
| Keyboard | **Meta+Ctrl+D** over a word |

If a phrase is selected (*New York*), triggering on one of its words looks up the whole phrase.
In the popover, **Escape** or a click outside closes it, **Enter** opens the dictionary entry,
and **Ctrl+C** copies the word.

## Install

Needs Tesseract with English data, and membership in the `input` group (gestures read
`/dev/input`; input is only read, never blocked, and keyboards are never opened).

```sh
sudo dnf install tesseract tesseract-langpack-eng libxkbcommon-devel                 # Fedora
sudo apt install tesseract-ocr tesseract-ocr-eng libxkbcommon-dev libwayland-dev    # Debian/Ubuntu
sudo pacman -S tesseract tesseract-data-eng libxkbcommon wayland                    # Arch

sudo usermod -aG input $USER   # then log out and back in

cargo build --release
target/release/tlum install    # into ~/.local/bin, with autostart and the shortcut where possible
tlum doctor                    # check what works on this desktop
```

`tlum uninstall` undoes `install`.

### Shortcut setup

KDE, GNOME and X11 need nothing more. Elsewhere, start tlum and bind `tlum trigger`:

| Desktop | Config |
| --- | --- |
| sway | `exec tlum` · `bindsym Mod4+Ctrl+d exec tlum trigger` |
| Hyprland | `exec-once = tlum` · `bind = SUPER CTRL, D, exec, tlum trigger` |
| niri | `spawn-at-startup "tlum"` · `binds { Mod+Ctrl+D { spawn "tlum" "trigger"; } }` |
| Bare X11 WM | `tlum &` in `.xinitrc` |

## Usage

```
tlum                 run the daemon
tlum trigger         look up the word under the pointer
tlum define WORD     show the popover for WORD
tlum doctor          report the backend, capture, devices and OCR
tlum config          print the settings in effect
tlum style --init    copy the built-in CSS to edit
```

`TLUM_DEBUG=1 tlum` logs each step and saves the OCR input to `$XDG_RUNTIME_DIR/tlum/`.

## Configuration

Settings live in `~/.config/tlum/config.json`. Every field is optional:

```json
{
  "hold_ms": 600,
  "three_finger_tap": true,
  "force_click": false,
  "mouse_hold": true,
  "mouse_button": null,
  "backend": "auto",
  "shortcut": "Meta+Ctrl+D",
  "language": "en",
  "ocr_language": "eng"
}
```

`mouse_button` takes `"side"`, `"extra"`, `"forward"`, `"back"` or `"middle"`. `backend` forces
`"kwin"`, `"x11"` or `"wayland"`. `ocr_language` accepts Tesseract combinations such as
`"eng+ukr"`.

The popover is styled with CSS: `~/.config/tlum/style.css` cascades over
[`assets/default.css`](assets/default.css) and reloads on save. Each part is a single class
(`.popover`, `.marker`, `.headword`, `.sense`, `.thesaurus`, …); `tlum style --default` lists
them all. Example themes are in [`assets/themes`](assets/themes); try one with
`TLUM_STYLE=assets/themes/paper.css tlum`.

## How it works

tlum reads raw evdev gestures, finds the pointer and captures a strip of screen through
whichever backend the desktop allows (KWin scripts, X11, wlroots screencopy, Hyprland IPC or
the GNOME portal), OCRs it with Tesseract, and draws the highlighted word and popover in a
transparent [Rune](https://github.com/anomalyco/rune) overlay. Definitions come from the Free
Dictionary API, Wiktionary and Wikipedia; only the word leaves your machine.

## Limitations

- Away from KDE and Hyprland, held presses open the popover on release rather than during the press.
- GNOME relies on XWayland and the Screenshot portal: slower (~0.5 s) and asks permission once.
- libinput also turns a three-finger tap into a middle click, which can paste. Set
  `"three_finger_tap": true` if you want such behaviour.
- Tiny, low-contrast or stylised text may not be recognised.
- Lookups need the network.

## License

MIT
