# Text field key bindings

Every chord `TextInput` binds ([`input::init`](../src/input.rs)), what it
does, and where that behaviour comes from. The composer is a multi-line field,
so most chords are defined by the unit they operate on:

| Unit | Meaning |
| --- | --- |
| character | one grapheme |
| word | a Unicode word boundary, whitespace skipped |
| row | one *rendered* row: a soft wrap counts, a hard `\n` counts |
| paragraph | the text between hard `\n` breaks; soft wraps are ignored |
| document | the whole field |

GPUI matches modifiers exactly, so a chord with no binding is swallowed rather
than falling back to the unmodified key. That is why the modified deletes are
bound explicitly.

## macOS

The reference is AppKit's `StandardKeyBinding.dict`
(`/System/Library/Frameworks/AppKit.framework/Resources/`), the table every
native text view consults. The selector column names the AppKit action a chord
maps to there; a row with no selector is a Waku decision, explained inline.

### Caret

| Chord | Does | AppKit selector |
| --- | --- | --- |
| `left` / `right` | one character; with a selection, collapse to its near edge | `moveLeft:` / `moveRight:` |
| `ctrl-b` / `ctrl-f` | one character | `moveBackward:` / `moveForward:` |
| `alt-left` / `alt-right` | one word | `moveWordLeft:` / `moveWordRight:` |
| `ctrl-alt-b` / `ctrl-alt-f` | one word | `moveWordBackward:` / `moveWordForward:` |
| `up` / `down` | one row, keeping the column goal across a short row; at the first or last row the key propagates | `moveUp:` / `moveDown:` |
| `ctrl-p` / `ctrl-n` | one row | `moveUp:` / `moveDown:` |
| `cmd-left` / `cmd-right` | row start / end | `moveToLeftEndOfLine:` / `moveToRightEndOfLine:` |
| `ctrl-a` / `ctrl-e` | paragraph start / end; stays put on repeat | `moveToBeginningOfParagraph:` / `moveToEndOfParagraph:` |
| `alt-up` / `alt-down` | paragraph start / end; on repeat, the previous / next paragraph | `moveParagraphBackward:` / `moveParagraphForward:` |
| `cmd-up` / `cmd-down` | document start / end | `moveToBeginningOfDocument:` / `moveToEndOfDocument:` |
| `home` / `end` | document start / end. Native views only scroll here; Waku moves the caret, as most editors do | — |

### Selection

Each chord extends from the selection's moving end.

| Chord | Extends to | AppKit selector |
| --- | --- | --- |
| `shift-left` / `shift-right` | one character | `moveLeftAndModifySelection:` |
| `ctrl-shift-b` / `ctrl-shift-f` | one character | `moveBackwardAndModifySelection:` |
| `alt-shift-left` / `alt-shift-right` | one word | `moveWordLeftAndModifySelection:` |
| `ctrl-alt-shift-b` / `ctrl-alt-shift-f` | one word | `moveWordBackwardAndModifySelection:` |
| `shift-up` / `shift-down` | one row; past the first or last row, the document edge | `moveUpAndModifySelection:` |
| `ctrl-shift-p` / `ctrl-shift-n` | one row | `moveUpAndModifySelection:` |
| `cmd-shift-left` / `cmd-shift-right` | row start / end | `moveToLeftEndOfLineAndModifySelection:` |
| `ctrl-shift-left` / `ctrl-shift-right` | row start / end | `moveToLeftEndOfLineAndModifySelection:` |
| `ctrl-shift-a` / `ctrl-shift-e` | paragraph start / end | `moveToBeginningOfParagraphAndModifySelection:` |
| `alt-shift-up` / `alt-shift-down` | paragraph start / end, stepping on repeat | `moveParagraphBackwardAndModifySelection:` |
| `cmd-shift-up` / `cmd-shift-down` | document start / end | `moveToBeginningOfDocumentAndModifySelection:` |
| `shift-home` / `shift-end` | document start / end | `moveToBeginningOfDocumentAndModifySelection:` |
| `cmd-a` | everything | `selectAll:` |

### Deleting

| Chord | Deletes | AppKit selector |
| --- | --- | --- |
| `backspace` / `delete` | one character back / forward, or the selection | `deleteBackward:` / `deleteForward:` |
| `shift-`, `ctrl-`, `ctrl-shift-` + either | the same one character; the modifiers are ignored | `deleteBackward:` / `deleteForward:` (`ctrl-backspace` is `deleteBackwardByDecomposingPreviousCharacter:`, which Waku approximates) |
| `ctrl-h` / `ctrl-d` | one character back / forward | `deleteBackward:` / `deleteForward:` |
| `alt-backspace`, `ctrl-alt-backspace` | one word back | `deleteWordBackward:` |
| `alt-delete` | one word forward | `deleteWordForward:` |
| `cmd-backspace` | to the row start; nothing at a row start | `deleteToBeginningOfLine:` |
| `cmd-delete` | to the row end. Unbound natively; mirrors `cmd-backspace` | — |
| `ctrl-k` | to the paragraph end; parked on a `\n`, takes the break and joins the next line up | `deleteToEndOfParagraph:` |
| `ctrl-u` | to the paragraph start; nothing at a paragraph start. Unbound natively; readline's `unix-line-discard` | — |

Every kill falls back to the hard line break when the field has no current
layout to read a row from, so a stale row can never cost more than the line.

### Enter, undo, escape

| Chord | Does |
| --- | --- |
| `enter` | submits; while a turn is running, queues a follow-up |
| `shift-enter`, `ctrl-enter`, `alt-enter` | insert a line break (`insertLineBreak:` / `insertNewlineIgnoringFieldEditor:`) |
| `cmd-enter` | steers with the draft, or injects the oldest queued follow-up |
| `cmd-z` / `cmd-shift-z` | undo / redo |
| `cmd-c` / `cmd-x` / `cmd-v` | copy / cut / paste |
| `escape` | clears fields that opt in via `clear_on_escape` (the search fields); otherwise propagates. The composer does not opt in: its clear also drops undo history, so Escape stops the turn or dismisses a popup instead |

### Deliberately unbound

| Chord | Why |
| --- | --- |
| `pageup` / `pagedown` | native views only scroll on these; leaving them unbound lets the transcript page |
| `shift-pageup` / `shift-pagedown`, `alt-pageup` / `alt-pagedown`, `ctrl-v` | page-wise caret motion needs the field's viewport height; not implemented yet |
| `ctrl-o`, `ctrl-t`, `ctrl-y`, `ctrl-l` | open-line, transpose, yank, centre; rare, and yank needs a kill ring |
| `ctrl-j` | AppKit's `insertNewline:`, the same selector as Enter, which would send |

## Windows and Linux

The shared desktop convention: `ctrl` for word motion, Home and End on the
row, the document ends one modifier up.

| Chord | Does |
| --- | --- |
| `home` / `end`, `shift-home` / `shift-end` | row start / end, move or select |
| `ctrl-home` / `ctrl-end`, `ctrl-shift-home` / `ctrl-shift-end` | document start / end, move or select |
| `ctrl-left` / `ctrl-right`, `ctrl-shift-left` / `ctrl-shift-right` | one word, move or select |
| `ctrl-backspace` / `ctrl-delete` | one word back / forward |
| `ctrl-y` | redo, alongside `ctrl-shift-z` |

Everything in the platform-neutral block above (arrows, shift-arrows, alt word
motion, the modified deletes, Enter and its variants, the clipboard chords)
applies here too.

## Popup contexts

While the composer's autocomplete popup is open (`ComposerAutocomplete >
TextInput`), `up`, `down`, `ctrl-p` and `ctrl-n` move its highlight, `enter`
and `tab` accept, and `escape` dismisses. The command palette, skills, model,
branch and settings search fields bind their list navigation the same way at
their own contexts.

## Checking by hand

Paste three paragraphs into the composer, the first long enough to soft-wrap
onto three rows, the second a few characters, and narrow the window:

```
The quick brown fox jumps over the lazy dog while the composer wraps this first paragraph across several rendered rows in a narrow window
short line
Third paragraph ends the draft
```

`cmd-right` from the start of the first paragraph must stop at the wrap;
`ctrl-e` must reach the `\n`. `down` from the end of the first row, through
`short line`, and back `up` must return to the same column. `ctrl-k` at
column 4 of the first paragraph must leave `The ` followed by the break.
