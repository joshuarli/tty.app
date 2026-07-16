#!/usr/bin/env bash

# Combined ANSI, indexed-color, and truecolor test.
# Source: https://gist.github.com/onaforeignshore/66c7b69a493f0e44a6f1e26186b72c64

set -u

printf '\n\033[38;5;231m System colors:\033[m\n'
for r in {0..2}; do
    for i in {0..15}; do
        if [[ "$r" == "1" ]]; then
            if [[ $i -gt 1 && $i -ne 4 ]]; then printf '\033[38;5;16m'; fi
            printf '\033[48;5;%sm    %03d    \033[m ' "$i" "$i"
        else
            printf '\033[48;5;%sm           \033[m ' "$i"
        fi
    done
    printf '\n'
done

printf '\n\033[38;5;231m 256 color extended mode\033[m (8-bit): Color cube, 6x6x6\n'
for i in {0..5}; do
    i=$(( i * 6 + 16 ))
    for r in {0..2}; do
        printf ' '
        for j in {0..5}; do
            j=$(( j * 36 ))
            for k in {0..5}; do
                val=$(( i + j + k ))
                if [[ "$r" == "1" ]]; then
                    case "$val" in
                        16|17|18|52|53|88) printf '\033[38;5;231m' ;;
                        *) printf '\033[38;5;16m' ;;
                    esac
                    printf '\033[48;5;%sm %03d \033[m' "$val" "$val"
                else
                    printf '\033[48;5;%sm     \033[m' "$val"
                fi
            done
            printf '   '
        done
        printf '\n'
    done
done

printf '\n\033[38;5;231m Grayscale ramp:\033[m\n'
for r in {0..2}; do
    if [[ "$r" == "1" ]]; then
        printf '\033[48;5;16m        016  \033[m'
    else
        printf '\033[48;5;16m             \033[m'
    fi
    for i in {232..255}; do
        if [[ "$r" == "1" ]]; then
            if [[ $i -gt 240 ]]; then printf '\033[38;5;16m'; fi
            printf '\033[48;5;%sm  %03d  \033[m' "$i" "$i"
        else
            printf '\033[48;5;%sm       \033[m' "$i"
        fi
    done
    if [[ "$r" == "1" ]]; then
        printf '\033[38;5;16m\033[48;5;231m  231   \033[m'
    else
        printf '\033[48;5;231m        \033[m'
    fi
    printf '\n'
done

printf '\n\033[38;5;196mR\033[38;5;40mG\033[38;5;21mB\033[38;5;231m 3-byte Truecolor mode\033[m: Color cube, 16x16x6\n'
for g in {0..15}; do
    valg=$(( g * 16 ))
    for r in 0 95 135 175 215 255; do
        for b in {0..31}; do
            valb=$(( b * 8 ))
            printf '\033[48;2;%d;%d;%dm \033[m' "$r" "$valg" "$valb"
        done
        printf ' '
    done
    printf '\n'
done

printf '\n\033[38;5;231m Grayscale ramp:\033[m\n'
for r in {0..1}; do
    printf '  '
    for g in {0..63}; do
        g=$(( g * 4 ))
        printf '\033[48;2;%d;%d;%dm   \033[m' "$g" "$g" "$g"
    done
    printf '\n'
done
printf '\n'
