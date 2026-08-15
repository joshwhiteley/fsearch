# fsearch shell integration for zsh.
#   source /path/to/fsearch.zsh
# Ctrl-T  — pick a file and insert its path at the cursor
# fcd     — cd into a picked directory

fsearch-file-widget() {
  local picked
  picked="$(fsearch --pick < /dev/tty)" || { zle reset-prompt; return }
  LBUFFER+="${(q)picked}"
  zle reset-prompt
}
zle -N fsearch-file-widget
bindkey '^T' fsearch-file-widget

fcd() {
  local picked
  picked="$(fsearch --pick "dir: $*")" || return
  cd "${picked%/}" || return
}
