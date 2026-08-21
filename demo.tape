# Demo recording for the README. Regenerate with:
#   ./docs/demo-setup.sh && vhs demo.tape
# (the setup script builds /tmp/fsearch-demo and pre-indexes it)
Output demo.gif

Set FontSize 18
Set Width 1400
Set Height 800
Set TypingSpeed 90ms
Set Padding 12

Env HOME "/tmp/fsearch-demo/home"
Env XDG_CONFIG_HOME "/tmp/fsearch-demo/xdg"
Env XDG_CACHE_HOME "/tmp/fsearch-demo/cache"
# vhs's recorder answers capability queries but renders none of the
# graphics protocols; halfblock cells record faithfully
Env FSEARCH_IMAGES "halfblocks"

Hide
Type "fsearch" Enter
Sleep 2s
Show

# fuzzy filename search — rich rows, badges, match highlighting
Type "invoice"
Sleep 2.5s

# syntax-highlighted code preview
Ctrl+U
Sleep 500ms
Type "mainrs"
Sleep 3s

# image preview (SVG rendered in the terminal)
Ctrl+U
Sleep 500ms
Type "logo"
Sleep 3s

# regex search inside file contents — matched line under each hit
Ctrl+U
Sleep 500ms
Type "> overdue"
Sleep 3.5s

# inline calculator
Ctrl+U
Sleep 500ms
Type "= 4200*2 + 11850"
Sleep 2.5s

Escape
Sleep 1s
