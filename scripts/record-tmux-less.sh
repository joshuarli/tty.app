#!/bin/zsh

set -e

mode=${1:-both}
output=${2:-target/tmux-less-44x148-${mode}.typescript}
root=${3:-$PWD}
socket="ttybaseline-$$"
session=baseline

if [[ "$mode" != both && "$mode" != sparse ]]; then
    print -u2 "usage: $0 [both|sparse] [output] [repo-root]"
    exit 2
fi

mkdir -p "${output:h}"

cleanup() {
    tmux -L "$socket" kill-server 2>/dev/null || true
}
trap cleanup EXIT

tmux -L "$socket" -f /dev/null new-session \
    -d -s "$session" -x 148 -y 44 -c "$root" /bin/zsh
tmux -L "$socket" set-option -g default-shell /bin/zsh
tmux -L "$socket" send-keys -t "$session" \
    "LESS= /usr/bin/less -f $root/Cargo.lock" C-m
tmux -L "$socket" split-window -h -t "$session" -c "$root" /bin/zsh
tmux -L "$socket" send-keys -t "$session" \
    "LESS= /usr/bin/less -f $root/Cargo.lock" C-m

sleep 1

expect <<EOF
log_user 0
set timeout 15
spawn script -q "$output" /bin/zsh -c {stty rows 44 columns 148; exec tmux -L $socket attach-session -t $session}
sleep 1

send " "
send " "
send " "
send " "
sleep 0.2

if {"$mode" eq "both"} {
    send "\002o"
    sleep 0.2
    send " "
    send " "
    send " "
    send " "
    sleep 0.2
}

send "\002d"
expect eof
EOF

print "recorded $mode workload: $output"
wc -c "$output"
