#!/usr/bin/env bash

# This file was originally taken from iTerm2's 24-bit color test and was
# published by John Morales at the source URL below.
# https://raw.githubusercontent.com/JohnMorales/dotfiles/master/colors/24-bit-color.sh

setBackgroundColor() {
    printf '\033[48;2;%s;%s;%sm' "$1" "$2" "$3"
}

resetOutput() {
    printf '\033[0m\n'
}

rainbowColor() {
    local h=$(( $1 / 43 ))
    local f=$(( $1 - 43 * h ))
    local t=$(( f * 255 / 43 ))
    local q=$(( 255 - t ))

    case "$h" in
        0) echo "255 $t 0" ;;
        1) echo "$q 255 0" ;;
        2) echo "0 255 $t" ;;
        3) echo "0 $q 255" ;;
        4) echo "$t 0 255" ;;
        5) echo "255 0 $q" ;;
        *) echo "0 0 0" ;;
    esac
}

for i in $(seq 0 127); do
    setBackgroundColor "$i" 0 0
    printf ' '
done
resetOutput

for i in $(seq 255 -1 128); do
    setBackgroundColor "$i" 0 0
    printf ' '
done
resetOutput

for i in $(seq 0 127); do
    setBackgroundColor 0 "$i" 0
    printf ' '
done
resetOutput

for i in $(seq 255 -1 128); do
    setBackgroundColor 0 "$i" 0
    printf ' '
done
resetOutput

for i in $(seq 0 127); do
    setBackgroundColor 0 0 "$i"
    printf ' '
done
resetOutput

for i in $(seq 255 -1 128); do
    setBackgroundColor 0 0 "$i"
    printf ' '
done
resetOutput

for i in $(seq 0 127); do
    setBackgroundColor $(rainbowColor "$i")
    printf ' '
done
resetOutput

for i in $(seq 255 -1 128); do
    setBackgroundColor $(rainbowColor "$i")
    printf ' '
done
resetOutput
