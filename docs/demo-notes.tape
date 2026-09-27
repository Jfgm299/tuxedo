# VHS script for the fork's notes demo GIF.
#
#   cargo build --release && vhs docs/demo-notes.tape  →  docs/demo-notes.gif
#
# Recorded in Catppuccin Macchiato (from docs/themes) on a copy of the sample
# todo list. Scenes:
#   1. Star a task: it jumps to the top of its priority group
#   2. Notes popup, new note, vim-style editing: lists continue on Enter,
#      Tab nests, long lines soft-wrap; the border follows the mode
#   3. Enter ticks a checkbox, :w saves
#   4. M shows the note rendered
#   5. z pins it next to the list
#   6. ? then Tab: the notes page of the help

Output docs/demo-notes.gif

Set FontSize 14
Set Width 1200
Set Height 720
Set Padding 12
Set Theme "Catppuccin Macchiato"
Set TypingSpeed 45ms
Set Shell "bash"

# Isolated config (theme + notes dir) and a fresh copy of the sample list, so
# the recording never touches the user's own files. Hidden so the recording
# opens straight into the TUI.
Hide
Type "export D=/tmp/tuxedo-notes-demo TUXEDO_NO_UPDATE_CHECK=1 && rm -rf $D && mkdir -p $D/cfg/tuxedo/themes $D/notes && cp docs/themes/catppuccin-macchiato.toml $D/cfg/tuxedo/themes/ && printf 'theme = Catppuccin Macchiato\nnotes_dir = %s/notes\n' $D > $D/cfg/tuxedo/config.toml && export XDG_CONFIG_HOME=$D/cfg && printf '(A) Call dentist to reschedule cleaning @phone +health\n(A) Finish Q2 board deck +work @laptop\n(B) Review pull requests for auth refactor +work @laptop\n(B) Draft hiring rubric for senior eng +work @laptop\n(B) Renew passport before summer trip +travel @errands\n(C) Order new running shoes +health @errands\nPick up dry cleaning @errands\nSchedule annual physical @phone +health\nPlan Saturday hike with K. @phone +personal\n' > $D/todo.txt && clear"
Enter
Sleep 500ms
Type "./target/release/tuxedo $D/todo.txt"
Enter
Sleep 1500ms
Show

Sleep 1500ms

# 1. Star "Finish Q2 board deck": it moves to the top of the (A) group.
Type "j"
Sleep 800ms
Type "*"
Sleep 2000ms

# 2. Open its notes, create "deck", edit it.
Type "o"
Sleep 1000ms
Type "n"
Sleep 500ms
Type "deck"
Sleep 400ms
Enter
Sleep 1200ms
Type "e"
Sleep 1000ms
# Below the title's blank line, into Insert mode.
Type "G"
Sleep 400ms
Type "o"
Sleep 600ms
Type "## Outline"
Enter
Enter
Type "- [ ] Revenue and burn vs. plan"
Enter
Sleep 400ms
Type "Hiring: the senior eng rubric, both open roles and the new offer bands"
Enter
Sleep 400ms
Tab
Sleep 500ms
Type "Pipeline numbers from the recruiter"
Enter
Sleep 400ms
# Enter on the empty nested item un-nests it.
Enter
Sleep 500ms
Type "Roadmap"
Sleep 800ms
Escape
Sleep 1200ms

# 3. Back up to "Revenue" (line 5), Enter ticks it, :w saves.
Type "5G"
Sleep 800ms
Enter
Sleep 1200ms
Type ":w"
Sleep 600ms
Enter
Sleep 1200ms

# 4. The note rendered.
Type "M"
Sleep 3000ms

# 5. Pin it to the right panel, next to the list.
Type "z"
Sleep 3000ms

# 6. Back to the list, help, Tab to the notes page.
Type "z"
Sleep 800ms
Type "?"
Sleep 1500ms
Tab
Sleep 3500ms
Escape
Sleep 1000ms
Type "q"
Sleep 500ms
