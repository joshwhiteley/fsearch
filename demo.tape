# Demo recording for the README. Regenerate with:
#   vhs demo.tape
# (expects the demo tree + env described in the tape to exist; see repo docs)
Output demo.gif

Set FontSize 18
Set Width 1400
Set Height 800
Set TypingSpeed 90ms
Set Padding 12

Env HOME "/private/tmp/claude-501/-Users-josh-file-search/bc474d2a-dc9c-497e-9bd8-7c38ff2e664e/scratchpad/demo/home"
Env XDG_CONFIG_HOME "/private/tmp/claude-501/-Users-josh-file-search/bc474d2a-dc9c-497e-9bd8-7c38ff2e664e/scratchpad/demo/xdg"
Env XDG_CACHE_HOME "/private/tmp/claude-501/-Users-josh-file-search/bc474d2a-dc9c-497e-9bd8-7c38ff2e664e/scratchpad/demo/cache"
# vhs's recorder answers capability queries but renders none of the
# graphics protocols; halfblock cells record faithfully
Env FSEARCH_IMAGES "halfblocks"

Hide
Type "fsearch" Enter
Sleep 2s
Show

# fuzzy filename search with match highlighting
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

# regex search inside file contents
Ctrl+U
Sleep 500ms
Type "> overdue"
Sleep 3.5s

Escape
Sleep 1s
