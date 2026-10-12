brew install tmux

if test $status -eq 0
    tmux -Lmnemos -CC
    exit
end
