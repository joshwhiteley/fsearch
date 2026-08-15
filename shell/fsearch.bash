# fsearch shell integration for bash.
#   source /path/to/fsearch.bash
# Ctrl-T  — pick a file and insert its path at the cursor
# fcd     — cd into a picked directory

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
