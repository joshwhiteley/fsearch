# fsearch shell integration for bash.
#   source /path/to/fsearch.bash
# Ctrl-T  — pick a file and insert its path at the cursor
# fcd     — cd into a picked directory
# Ctrl-R  — fuzzy-pick a history command (replaces the default reverse-search)
#           skip rebinding by commenting out the bind -x line at the bottom

__fsearch_file_widget() {
  local picked
  picked="$(fsearch --pick < /dev/tty)" || return
  READLINE_LINE="${READLINE_LINE:0:READLINE_POINT}${picked@Q}${READLINE_LINE:READLINE_POINT}"
  READLINE_POINT=$((READLINE_POINT + ${#picked} + 2))
}
bind -x '"\C-t": __fsearch_file_widget'

fcd() {
  local picked
  picked="$(fsearch --pick "dir: $*")" || return
  cd "${picked%/}" || return
}

__fsearch_history_widget() {
  local picked
  picked="$(history | sed 's/^ *[0-9]* *//' | awk '!seen[$0]++' | fsearch --filter)" || return
  READLINE_LINE="$picked"
  READLINE_POINT=${#picked}
}
bind -x '"\C-r": __fsearch_history_widget'
