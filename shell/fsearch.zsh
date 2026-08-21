# fsearch shell integration for zsh.
#   source /path/to/fsearch.zsh
# Ctrl-T  — pick a file and insert its path at the cursor
# fcd     — cd into a picked directory
# Ctrl-R  — fuzzy-pick a history command (replaces the default reverse-search)
#           skip rebinding by commenting out the bindkey line at the bottom

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

fsearch-history-widget() {
  local picked
  picked="$(fc -rl 1 | sed 's/^ *[0-9]*\** *//' | awk '!seen[$0]++' | fsearch --filter)" || { zle reset-prompt; return }
  BUFFER="$picked"
  CURSOR=$#BUFFER
  zle reset-prompt
}
zle -N fsearch-history-widget
bindkey '^R' fsearch-history-widget
